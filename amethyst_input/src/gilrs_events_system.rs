use std::{
    fmt,
    marker::PhantomData, 
    collections::{HashMap, hash_map::DefaultHasher},
    hash::{Hash, Hasher}
};

use derivative::Derivative;
use derive_new::new;
use gilrs::{Gilrs, Button, Axis, Event, EventType, GamepadId};

use amethyst_core::{
    ecs::prelude::{System, SystemData, World, Write},
    shrev::EventChannel,
    SystemDesc,
};

use super::{
    controller::{ControllerAxis, ControllerButton, ControllerEvent},
    BindingTypes, InputEvent, InputHandler,
};

/// A collection of errors that can occur in the SDL system.
#[derive(Debug)]
pub enum GilrsSystemError {
    /// Failure initializing SDL context
    ContextInit(String),
    /// Failure initializing SDL controller subsystem
    ControllerSubsystemInit(String),
}

impl fmt::Display for GilrsSystemError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            GilrsSystemError::ContextInit(ref msg) => write!(f, "Failed to initialize SDL: {}", msg),
            GilrsSystemError::ControllerSubsystemInit(ref msg) => {
                write!(f, "Failed to initialize SDL controller subsystem: {}", msg)
            }
        }
    }
}

/// Builds a `SdlEventsSystem`.
#[derive(Derivative, Debug, new)]
#[derivative(Default(bound = ""))]
pub struct GilrsEventsSystemDesc<T>
where
    T: BindingTypes,
{
    marker: PhantomData<T>,
}

impl<'a, 'b, T> SystemDesc<'a, 'b, GilrsEventsSystem<T>> for GilrsEventsSystemDesc<T>
where
    T: BindingTypes,
{
    fn build(self, world: &mut World) -> GilrsEventsSystem<T> {
        <GilrsEventsSystem<T> as System<'_>>::SystemData::setup(world);

        GilrsEventsSystem::new(world)
            .unwrap_or_else(|e| panic!("Failed to build SdlEventsSystem. Error: {}", e))
    }
}

/// A system that pumps SDL events into the `amethyst_input` APIs.
#[allow(missing_debug_implementations)]
pub struct GilrsEventsSystem<T: BindingTypes> {
    gilrs_handle: Gilrs,
    opened_controllers: HashMap<GamepadId, u32>,
    marker: PhantomData<T>,
}

type GilrsEventsData<'a, T> = (
    Write<'a, InputHandler<T>>,
    Write<'a, EventChannel<InputEvent<T>>>,
);

impl<'a, T: BindingTypes> System<'a> for GilrsEventsSystem<T> {
    type SystemData = GilrsEventsData<'a, T>;

    fn run(&mut self, (mut handler, mut output): Self::SystemData) {
        while let Some(Event { id, event, time: _ }) = self.gilrs_handle.next_event() {
            self.handle_gilrs_event(&id, &event, &mut handler, &mut output);
        }
    }
}

impl<T: BindingTypes> GilrsEventsSystem<T> {
    /// Creates a new instance of this system with the provided controller mappings.
    pub fn new(
        world: &mut World,
    ) -> Result<Self, GilrsSystemError> {
        let gilrs_handle: Gilrs = Gilrs::new().unwrap();
        GilrsEventsData::<T>::setup(world);
        let mut sys = GilrsEventsSystem {
            gilrs_handle,
            opened_controllers: HashMap::new(),
            marker: PhantomData
        };
        let (mut handler, mut output) = GilrsEventsData::fetch(world);
        sys.initialize_controllers(&mut handler, &mut output);
        Ok(sys)
    }

    fn handle_gilrs_event(
        &mut self,
        gamepad_id: &GamepadId,
        event_type: &EventType,
        handler: &mut InputHandler<T>,
        output: &mut EventChannel<InputEvent<T>>,
    ) {
        use self::ControllerEvent::*;

        if let Some(idx) = self.opened_controllers.get(gamepad_id) {
            match *event_type {
                EventType::AxisChanged(axis, value, _code) => {
                    handler.send_controller_event(
                        &ControllerAxisMoved {
                            which: *idx,
                            axis: axis.into(),
                            value: value,
                        },
                        output,
                    );
                }
                EventType::ButtonReleased(button, _code) => {
                    handler.send_controller_event(
                        &ControllerButtonReleased {
                            which: *idx,
                            button: button.into(),
                        },
                        output,
                    );
                }
                EventType::ButtonPressed(button, _code) => {
                    handler.send_controller_event(
                        &ControllerButtonPressed {
                            which: *idx,
                            button: button.into(),
                        },
                        output,
                    );
                }
                EventType::Disconnected => {
                    if let Some(idx) = self.close_controller(*gamepad_id) {
                        handler.send_controller_event(&ControllerDisconnected {which: idx}, output);
                    }
                }
                EventType::Connected => {
                    if let Some(idx) = self.open_controller(*gamepad_id) {
                        handler.send_controller_event(&ControllerConnected {which: idx}, output);
                    }
                }
                _ => {}
            }
        } else {
            match *event_type {
                EventType::Connected => {
                    if let Some(idx) = self.open_controller(*gamepad_id) {
                        handler.send_controller_event(&ControllerConnected {which: idx}, output);
                    }
                }
                _ => {}
            }
        }
    }

    fn open_controller(&mut self, which: GamepadId) -> Option<u32> {
        match self.gilrs_handle.connected_gamepad(which) {
            Some(_) => {
                let idx = self.my_hash(which) as u32;
                self.opened_controllers.insert(which, idx);
                Some(idx)
            },
            None => None
        }
    }

    fn close_controller(&mut self, which: GamepadId) ->Option<u32> {
        self.opened_controllers.remove(&which)
    }

    fn initialize_controllers(
        &mut self,
        handler: &mut InputHandler<T>,
        output: &mut EventChannel<InputEvent<T>>,
    ) {
        use crate::controller::ControllerEvent::ControllerConnected;

        for (_id, gamepad) in self.gilrs_handle.gamepads() {
            let idx = self.my_hash(gamepad.id()) as u32;
            self.opened_controllers.insert(gamepad.id(), idx);
            handler.send_controller_event(&ControllerConnected {which: idx}, output);
        }
    }

    fn my_hash<U>(&self, obj: U) -> u64
    where
        U: Hash,
    {
        let mut hasher = DefaultHasher::new();
        obj.hash(&mut hasher);
        hasher.finish()
    }
}

impl From<Button> for ControllerButton {
    fn from(button: Button) -> Self {
        match button {
            Button::South => ControllerButton::A,
            Button::East => ControllerButton::B,
            Button::West => ControllerButton::X,
            Button::North => ControllerButton::Y,
            Button::DPadDown => ControllerButton::DPadDown,
            Button::DPadLeft => ControllerButton::DPadLeft,
            Button::DPadRight => ControllerButton::DPadRight,
            Button::DPadUp => ControllerButton::DPadUp,
            Button::LeftTrigger => ControllerButton::LeftShoulder,
            Button::RightTrigger => ControllerButton::RightShoulder,
            Button::LeftThumb => ControllerButton::LeftStick,
            Button::RightThumb => ControllerButton::RightStick,
            Button::Select => ControllerButton::Back,
            Button::Start => ControllerButton::Start,
            Button::Mode => ControllerButton::Guide,
            Button::LeftTrigger2 =>ControllerButton::LeftTrigger, 
            Button::RightTrigger2 => ControllerButton::RightTrigger,
            _ => ControllerButton::Unknown,
        }
    }
}

impl From<Axis> for ControllerAxis {
    fn from(axis: Axis) -> Self {
        match axis {
            Axis::LeftStickX => ControllerAxis::LeftX,
            Axis::LeftStickY => ControllerAxis::LeftY,
            Axis::RightStickX => ControllerAxis::RightX,
            Axis::RightStickY => ControllerAxis::RightY,
            Axis::LeftZ => ControllerAxis::LeftTrigger,
            Axis::RightZ => ControllerAxis::RightTrigger,
            _ => ControllerAxis::Unknown,
        }
    }
}
