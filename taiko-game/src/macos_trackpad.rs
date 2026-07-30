use std::time::Instant;

use anyhow::Result;
use rhythm_mode_taiko::TaikoAction;

use crate::controller::ControllerSlot;

#[cfg(any(target_os = "macos", test))]
const MAX_CONTACTS_PER_FRAME: usize = 32;
#[cfg(target_os = "macos")]
const TRACKPAD_QUEUE_CAPACITY: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MacTrackpadAvailability {
    Available,
    Unavailable(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MacTrackpadHit {
    pub(crate) slot: ControllerSlot,
    pub(crate) action: TaikoAction,
    pub(crate) observed_at: Instant,
}

#[derive(Debug, Default)]
pub(crate) struct MacTrackpadDrain {
    pub(crate) hits: Vec<MacTrackpadHit>,
    pub(crate) dropped: u64,
}

pub(crate) struct MacTrackpad {
    backend: Option<imp::Backend>,
    availability: MacTrackpadAvailability,
}

impl MacTrackpad {
    pub(crate) fn open() -> Self {
        match imp::Backend::open() {
            Ok(backend) => Self {
                backend: Some(backend),
                availability: MacTrackpadAvailability::Available,
            },
            Err(error) => Self {
                backend: None,
                availability: MacTrackpadAvailability::Unavailable(format!("{error:#}")),
            },
        }
    }

    pub(crate) fn availability(&self) -> MacTrackpadAvailability {
        self.availability.clone()
    }

    pub(crate) fn set_target(&mut self, target: Option<ControllerSlot>) -> Result<()> {
        if let Some(backend) = &mut self.backend {
            backend.set_target(target)?;
        }
        Ok(())
    }

    pub(crate) fn drain(&mut self) -> Result<MacTrackpadDrain> {
        self.backend
            .as_mut()
            .map_or_else(|| Ok(MacTrackpadDrain::default()), imp::Backend::drain)
    }

    #[cfg(all(test, target_os = "macos"))]
    fn diagnostics(&self) -> (u64, u64, u64) {
        self.backend
            .as_ref()
            .map_or((0, 0, 0), imp::Backend::diagnostics)
    }

    pub(crate) fn shutdown(&mut self) -> Result<()> {
        if let Some(mut backend) = self.backend.take() {
            backend.shutdown()
        } else {
            Ok(())
        }
    }
}

impl Drop for MacTrackpad {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Copy)]
struct TrackpadContact {
    identity: i32,
    normalized_x: f32,
    touching: bool,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingHit {
    slot: ControllerSlot,
    action: TaikoAction,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug)]
struct ReducedFrame {
    hits: [Option<PendingHit>; MAX_CONTACTS_PER_FRAME],
    len: usize,
}

#[cfg(any(target_os = "macos", test))]
impl ReducedFrame {
    fn new() -> Self {
        Self {
            hits: [None; MAX_CONTACTS_PER_FRAME],
            len: 0,
        }
    }

    fn push(&mut self, hit: PendingHit) {
        debug_assert!(self.len < self.hits.len());
        self.hits[self.len] = Some(hit);
        self.len += 1;
    }

    fn iter(&self) -> impl Iterator<Item = PendingHit> + '_ {
        self.hits[..self.len]
            .iter()
            .map(|hit| hit.expect("filled frame entries contain hits"))
    }
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug)]
struct ContactReducer {
    active_identities: [i32; MAX_CONTACTS_PER_FRAME],
    active_len: usize,
    target: Option<ControllerSlot>,
    suppress_until_neutral: bool,
}

#[cfg(any(target_os = "macos", test))]
impl ContactReducer {
    fn new() -> Self {
        Self {
            active_identities: [0; MAX_CONTACTS_PER_FRAME],
            active_len: 0,
            target: None,
            suppress_until_neutral: false,
        }
    }

    fn set_target(&mut self, target: Option<ControllerSlot>) -> bool {
        if self.target == target {
            return false;
        }
        self.target = target;
        self.suppress_until_neutral = target.is_some() && self.active_len > 0;
        true
    }

    fn invalidate_frame(&mut self) {
        self.active_len = 0;
        self.suppress_until_neutral = self.target.is_some();
    }

    fn process_frame(&mut self, contacts: &[TrackpadContact]) -> ReducedFrame {
        let mut current = [0; MAX_CONTACTS_PER_FRAME];
        let mut current_len = 0;
        let mut reduced = ReducedFrame::new();

        for contact in contacts.iter().filter(|contact| contact.touching) {
            if current[..current_len].contains(&contact.identity) {
                continue;
            }
            current[current_len] = contact.identity;
            current_len += 1;

            let is_new = !self.active_identities[..self.active_len].contains(&contact.identity);
            if is_new && !self.suppress_until_neutral {
                if let (Some(slot), Some(action)) =
                    (self.target, action_for_normalized_x(contact.normalized_x))
                {
                    reduced.push(PendingHit { slot, action });
                }
            }
        }

        self.active_identities[..current_len].copy_from_slice(&current[..current_len]);
        self.active_len = current_len;
        if self.suppress_until_neutral && current_len == 0 {
            self.suppress_until_neutral = false;
        }
        reduced
    }
}

#[cfg(any(target_os = "macos", test))]
fn action_for_normalized_x(normalized_x: f32) -> Option<TaikoAction> {
    if !normalized_x.is_finite() {
        return None;
    }
    let zone = (normalized_x.clamp(0.0, 1.0) * 4.0).floor() as usize;
    Some(crate::drum_surface::DRUM_SURFACE_ACTIONS[zone.min(3)])
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::{c_char, c_double, c_int, c_void, CStr};
    use std::mem;
    use std::ptr;
    use std::slice;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
    use std::sync::{Arc, Mutex, OnceLock};
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    use anyhow::{anyhow, bail, Context, Result};

    use super::{
        ContactReducer, MacTrackpadDrain, MacTrackpadHit, TrackpadContact, MAX_CONTACTS_PER_FRAME,
        TRACKPAD_QUEUE_CAPACITY,
    };
    use crate::controller::ControllerSlot;

    const MULTITOUCH_FRAMEWORK: &CStr =
        c"/System/Library/PrivateFrameworks/MultitouchSupport.framework/MultitouchSupport";

    type MtDeviceRef = *mut c_void;
    type ContactFrameCallback =
        unsafe extern "C" fn(MtDeviceRef, *const MtTouch, c_int, c_double, c_int);
    type DeviceIsAvailable = unsafe extern "C" fn() -> bool;
    type DeviceCreateDefault = unsafe extern "C" fn() -> MtDeviceRef;
    type RegisterContactFrameCallback = unsafe extern "C" fn(MtDeviceRef, ContactFrameCallback);
    type UnregisterContactFrameCallback = unsafe extern "C" fn(MtDeviceRef, ContactFrameCallback);
    type DeviceStart = unsafe extern "C" fn(MtDeviceRef, c_int) -> c_int;
    type DeviceStop = unsafe extern "C" fn(MtDeviceRef) -> c_int;
    type DeviceRelease = unsafe extern "C" fn(MtDeviceRef);
    type CfStringRef = *const c_void;

    const CF_RUN_LOOP_FINISHED: c_int = 1;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFRunLoopDefaultMode: CfStringRef;
        fn CFRunLoopRunInMode(
            mode: CfStringRef,
            seconds: c_double,
            return_after_source_handled: u8,
        ) -> c_int;
    }

    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    struct MtPoint {
        x: f32,
        y: f32,
    }

    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    struct MtVector {
        position: MtPoint,
        velocity: MtPoint,
    }

    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    struct MtTouch {
        frame: i32,
        timestamp: f64,
        identifier: i32,
        state: i32,
        finger_id: i32,
        hand_id: i32,
        normalized_position: MtVector,
        z_total: f32,
        reserved_field9: i32,
        angle: f32,
        major_axis: f32,
        minor_axis: f32,
        absolute_position: MtVector,
        field14: i32,
        field15: i32,
        z_density: f32,
    }

    impl MtTouch {
        fn is_touching(self) -> bool {
            matches!(self.state, 3 | 4)
        }
    }

    struct DynamicLibrary(*mut c_void);

    impl DynamicLibrary {
        fn open(path: &CStr) -> Result<Self> {
            // SAFETY: `path` is a static, NUL-terminated C string. The returned
            // handle is owned by this wrapper and closed exactly once in Drop.
            let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
            if handle.is_null() {
                bail!(
                    "could not load MultitouchSupport: {}",
                    dynamic_loader_error()
                );
            }
            Ok(Self(handle))
        }

        fn symbol<T: Copy>(&self, name: &CStr) -> Result<T> {
            // SAFETY: Clearing and reading dlerror follows the dlsym contract.
            // Each requested symbol is immediately converted to its declared C
            // function-pointer type, whose ABI is checked against the reversed-
            // engineered framework declarations in this module.
            unsafe {
                libc::dlerror();
                let symbol = libc::dlsym(self.0, name.as_ptr());
                let error = libc::dlerror();
                if !error.is_null() {
                    bail!(
                        "MultitouchSupport symbol {} is unavailable: {}",
                        name.to_string_lossy(),
                        CStr::from_ptr(error).to_string_lossy()
                    );
                }
                if symbol.is_null() {
                    bail!(
                        "MultitouchSupport symbol {} resolved to null",
                        name.to_string_lossy()
                    );
                }
                if mem::size_of::<T>() != mem::size_of::<*mut c_void>() {
                    bail!(
                        "unexpected function-pointer size for {}",
                        name.to_string_lossy()
                    );
                }
                Ok(mem::transmute_copy(&symbol))
            }
        }
    }

    impl Drop for DynamicLibrary {
        fn drop(&mut self) {
            // SAFETY: This is the live handle returned by dlopen and ownership is
            // unique to this wrapper.
            unsafe {
                libc::dlclose(self.0);
            }
        }
    }

    struct Api {
        _library: DynamicLibrary,
        device_is_available: DeviceIsAvailable,
        device_create_default: DeviceCreateDefault,
        register_contact_frame_callback: RegisterContactFrameCallback,
        unregister_contact_frame_callback: UnregisterContactFrameCallback,
        device_start: DeviceStart,
        device_stop: DeviceStop,
        device_release: DeviceRelease,
    }

    impl Api {
        fn load() -> Result<Self> {
            let library = DynamicLibrary::open(MULTITOUCH_FRAMEWORK)?;
            Ok(Self {
                device_is_available: library.symbol(c"MTDeviceIsAvailable")?,
                device_create_default: library.symbol(c"MTDeviceCreateDefault")?,
                register_contact_frame_callback: library
                    .symbol(c"MTRegisterContactFrameCallback")?,
                unregister_contact_frame_callback: library
                    .symbol(c"MTUnregisterContactFrameCallback")?,
                device_start: library.symbol(c"MTDeviceStart")?,
                device_stop: library.symbol(c"MTDeviceStop")?,
                device_release: library.symbol(c"MTDeviceRelease")?,
                _library: library,
            })
        }
    }

    struct CallbackState {
        reducer: ContactReducer,
        sender: SyncSender<MacTrackpadHit>,
        dropped: Arc<AtomicU64>,
        #[cfg(test)]
        frames_seen: Arc<AtomicU64>,
        #[cfg(test)]
        contacts_seen: Arc<AtomicU64>,
        #[cfg(test)]
        touch_states_seen: Arc<AtomicU64>,
    }

    impl CallbackState {
        fn process(&mut self, touches: &[MtTouch], observed_at: Instant) {
            #[cfg(test)]
            {
                self.frames_seen.fetch_add(1, Ordering::Relaxed);
                self.contacts_seen
                    .fetch_add(touches.len() as u64, Ordering::Relaxed);
                for touch in touches {
                    if let Ok(state) = u32::try_from(touch.state) {
                        if state < u64::BITS {
                            self.touch_states_seen
                                .fetch_or(1_u64 << state, Ordering::Relaxed);
                        }
                    }
                }
            }
            if touches.len() > MAX_CONTACTS_PER_FRAME {
                self.reducer.invalidate_frame();
                self.dropped.fetch_add(1, Ordering::Relaxed);
                return;
            }

            let mut contacts = [TrackpadContact {
                identity: 0,
                normalized_x: 0.0,
                touching: false,
            }; MAX_CONTACTS_PER_FRAME];
            for (contact, touch) in contacts.iter_mut().zip(touches.iter().copied()) {
                *contact = TrackpadContact {
                    identity: touch.identifier,
                    normalized_x: touch.normalized_position.position.x,
                    touching: touch.is_touching(),
                };
            }

            let reduced = self.reducer.process_frame(&contacts[..touches.len()]);
            for hit in reduced.iter() {
                let event = MacTrackpadHit {
                    slot: hit.slot,
                    action: hit.action,
                    observed_at,
                };
                match self.sender.try_send(event) {
                    Ok(()) => {}
                    Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                        self.dropped.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }
    }

    type SharedCallbackState = Arc<Mutex<CallbackState>>;

    fn callback_registry() -> &'static Mutex<Option<SharedCallbackState>> {
        static REGISTRY: OnceLock<Mutex<Option<SharedCallbackState>>> = OnceLock::new();
        REGISTRY.get_or_init(|| Mutex::new(None))
    }

    unsafe extern "C" fn contact_frame_callback(
        _device: MtDeviceRef,
        touches: *const MtTouch,
        touch_count: c_int,
        _timestamp: c_double,
        _frame: c_int,
    ) {
        let Some(touch_count) = usize::try_from(touch_count).ok() else {
            return;
        };
        let touch_slice = if touch_count == 0 {
            &[]
        } else {
            if touches.is_null() {
                return;
            }
            // SAFETY: MultitouchSupport guarantees `touches` addresses an array
            // of `touch_count` MTTouch values for the duration of this callback.
            unsafe { slice::from_raw_parts(touches, touch_count) }
        };
        let shared = callback_registry()
            .lock()
            .ok()
            .and_then(|registry| registry.as_ref().cloned());
        if let Some(shared) = shared {
            if let Ok(mut state) = shared.lock() {
                state.process(touch_slice, Instant::now());
            }
        }
    }

    pub(super) struct Backend {
        state: SharedCallbackState,
        receiver: Receiver<MacTrackpadHit>,
        dropped: Arc<AtomicU64>,
        #[cfg(test)]
        frames_seen: Arc<AtomicU64>,
        #[cfg(test)]
        contacts_seen: Arc<AtomicU64>,
        #[cfg(test)]
        touch_states_seen: Arc<AtomicU64>,
        current_target: Option<ControllerSlot>,
        shutdown_sender: Option<SyncSender<()>>,
        worker: Option<JoinHandle<Result<()>>>,
    }

    impl Backend {
        pub(super) fn open() -> Result<Self> {
            let (sender, receiver) = mpsc::sync_channel(TRACKPAD_QUEUE_CAPACITY);
            let dropped = Arc::new(AtomicU64::new(0));
            #[cfg(test)]
            let frames_seen = Arc::new(AtomicU64::new(0));
            #[cfg(test)]
            let contacts_seen = Arc::new(AtomicU64::new(0));
            #[cfg(test)]
            let touch_states_seen = Arc::new(AtomicU64::new(0));
            let state = Arc::new(Mutex::new(CallbackState {
                reducer: ContactReducer::new(),
                sender,
                dropped: Arc::clone(&dropped),
                #[cfg(test)]
                frames_seen: Arc::clone(&frames_seen),
                #[cfg(test)]
                contacts_seen: Arc::clone(&contacts_seen),
                #[cfg(test)]
                touch_states_seen: Arc::clone(&touch_states_seen),
            }));

            {
                let mut registry = callback_registry()
                    .lock()
                    .map_err(|_| anyhow!("Mac trackpad callback registry is poisoned"))?;
                if registry.is_some() {
                    bail!("only one Mac trackpad source can be open per process");
                }
                *registry = Some(Arc::clone(&state));
            }

            let (startup_sender, startup_receiver) = mpsc::sync_channel(1);
            let (shutdown_sender, shutdown_receiver) = mpsc::sync_channel(1);
            let worker = match thread::Builder::new()
                .name("taiko-mac-trackpad".to_owned())
                .spawn(move || run_device_worker(startup_sender, shutdown_receiver))
            {
                Ok(worker) => worker,
                Err(error) => {
                    clear_registry_if_same(&state);
                    return Err(error).context("could not spawn the Mac trackpad event loop");
                }
            };

            let startup_error = match startup_receiver.recv() {
                Ok(Ok(())) => None,
                Ok(Err(reason)) => Some(anyhow!(reason)),
                Err(_) => Some(anyhow!(
                    "Mac trackpad event loop exited before reporting startup"
                )),
            };
            if let Some(error) = startup_error {
                let worker_result = worker
                    .join()
                    .map_err(|_| anyhow!("Mac trackpad event loop panicked"));
                clear_registry_if_same(&state);
                if let Ok(Err(worker_error)) = worker_result {
                    return Err(worker_error);
                }
                return Err(error);
            }

            Ok(Self {
                state,
                receiver,
                dropped,
                #[cfg(test)]
                frames_seen,
                #[cfg(test)]
                contacts_seen,
                #[cfg(test)]
                touch_states_seen,
                current_target: None,
                shutdown_sender: Some(shutdown_sender),
                worker: Some(worker),
            })
        }

        pub(super) fn set_target(&mut self, target: Option<ControllerSlot>) -> Result<()> {
            if self.current_target == target {
                return Ok(());
            }
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow!("Mac trackpad callback state is poisoned"))?;
            while self.receiver.try_recv().is_ok() {}
            state.reducer.set_target(target);
            self.current_target = target;
            Ok(())
        }

        pub(super) fn drain(&mut self) -> Result<MacTrackpadDrain> {
            let _callback_barrier = self
                .state
                .lock()
                .map_err(|_| anyhow!("Mac trackpad callback state is poisoned"))?;
            let mut hits = Vec::with_capacity(TRACKPAD_QUEUE_CAPACITY);
            while let Ok(hit) = self.receiver.try_recv() {
                hits.push(hit);
            }
            Ok(MacTrackpadDrain {
                hits,
                dropped: self.dropped.swap(0, Ordering::Relaxed),
            })
        }

        #[cfg(test)]
        pub(super) fn diagnostics(&self) -> (u64, u64, u64) {
            (
                self.frames_seen.load(Ordering::Relaxed),
                self.contacts_seen.load(Ordering::Relaxed),
                self.touch_states_seen.load(Ordering::Relaxed),
            )
        }

        pub(super) fn shutdown(&mut self) -> Result<()> {
            let Some(worker) = self.worker.take() else {
                return Ok(());
            };
            self.current_target = None;
            if let Ok(mut state) = self.state.lock() {
                state.reducer.set_target(None);
            }

            if let Some(shutdown_sender) = self.shutdown_sender.take() {
                match shutdown_sender.try_send(()) {
                    Ok(()) | Err(TrySendError::Full(())) => {}
                    Err(TrySendError::Disconnected(())) => {}
                }
            }
            let worker_result = worker.join();
            clear_registry_if_same(&self.state);
            worker_result.map_err(|_| anyhow!("Mac trackpad event loop panicked"))?
        }
    }

    impl Drop for Backend {
        fn drop(&mut self) {
            let _ = self.shutdown();
        }
    }

    fn run_device_worker(
        startup_sender: SyncSender<std::result::Result<(), String>>,
        shutdown_receiver: Receiver<()>,
    ) -> Result<()> {
        let mut device = match RunningDevice::start() {
            Ok(device) => device,
            Err(error) => {
                let reason = format!("{error:#}");
                let _ = startup_sender.send(Err(reason));
                return Err(error);
            }
        };
        if startup_sender.send(Ok(())).is_err() {
            return device
                .shutdown()
                .context("Mac trackpad owner disappeared during startup");
        }

        loop {
            match shutdown_receiver.try_recv() {
                Ok(()) | Err(TryRecvError::Disconnected) => break,
                Err(TryRecvError::Empty) => {}
            }
            // SAFETY: This worker owns the thread on which the multitouch device
            // was registered. Running its Core Foundation loop gives the private
            // driver a live delivery context without taking focus from the TUI.
            let status = unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.01, 0) };
            if status == CF_RUN_LOOP_FINISHED {
                thread::sleep(Duration::from_millis(1));
            }
        }

        device.shutdown()
    }

    struct RunningDevice {
        api: Api,
        device: MtDeviceRef,
        running: bool,
    }

    impl RunningDevice {
        fn start() -> Result<Self> {
            let api = Api::load()?;
            // SAFETY: The dynamically resolved function uses the declared C ABI
            // and takes no arguments.
            if !unsafe { (api.device_is_available)() } {
                bail!("this Mac has no default multitouch device");
            }
            // SAFETY: The framework owns the returned device object until the
            // matching MTDeviceRelease call in shutdown or rollback below.
            let device = unsafe { (api.device_create_default)() };
            if device.is_null() {
                bail!("MultitouchSupport could not create the default trackpad");
            }

            // SAFETY: The device is valid, the callback has the exact C ABI, and
            // the registry keeps all callback state alive until after stop.
            unsafe {
                (api.register_contact_frame_callback)(device, contact_frame_callback);
            }
            // SAFETY: The registered device is valid and not already running.
            let start_status = unsafe { (api.device_start)(device, 0) };
            if start_status != 0 {
                // SAFETY: Roll back registration and ownership in reverse order.
                unsafe {
                    (api.unregister_contact_frame_callback)(device, contact_frame_callback);
                    (api.device_release)(device);
                }
                bail!("MTDeviceStart failed with OSStatus {start_status}");
            }

            Ok(Self {
                api,
                device,
                running: true,
            })
        }

        fn shutdown(&mut self) -> Result<()> {
            if !self.running {
                return Ok(());
            }
            // SAFETY: Setup registered this exact callback on this live device.
            unsafe {
                (self.api.unregister_contact_frame_callback)(self.device, contact_frame_callback);
            }
            // SAFETY: The device is running and remains owned until release below.
            let stop_status = unsafe { (self.api.device_stop)(self.device) };
            // SAFETY: Stop has quiesced callbacks and this is the matching release
            // for MTDeviceCreateDefault.
            unsafe {
                (self.api.device_release)(self.device);
            }
            self.device = ptr::null_mut();
            self.running = false;
            if stop_status == 0 {
                Ok(())
            } else {
                Err(anyhow!("MTDeviceStop failed with OSStatus {stop_status}"))
            }
        }
    }

    impl Drop for RunningDevice {
        fn drop(&mut self) {
            let _ = self.shutdown();
        }
    }

    fn clear_registry_if_same(state: &SharedCallbackState) {
        if let Ok(mut registry) = callback_registry().lock() {
            if registry
                .as_ref()
                .is_some_and(|registered| Arc::ptr_eq(registered, state))
            {
                *registry = None;
            }
        }
    }

    fn dynamic_loader_error() -> String {
        // SAFETY: dlerror returns either null or a process-owned NUL-terminated
        // string that remains valid until the next loader call on this thread.
        unsafe {
            let error = libc::dlerror();
            if error.is_null() {
                "unknown dynamic-loader error".to_owned()
            } else {
                CStr::from_ptr(error as *const c_char)
                    .to_string_lossy()
                    .into_owned()
            }
        }
    }

    #[cfg(test)]
    mod ffi_tests {
        use std::mem::{align_of, offset_of, size_of};
        use std::sync::atomic::AtomicU64;
        use std::sync::{mpsc, Arc};
        use std::time::Instant;

        use rhythm_mode_taiko::TaikoAction;

        use super::{CallbackState, ContactReducer, MtPoint, MtTouch, MtVector};
        use crate::controller::ControllerSlot;

        #[test]
        fn mt_touch_layout_matches_the_reverse_engineered_macos_abi() {
            assert_eq!(size_of::<MtTouch>(), 96);
            assert_eq!(align_of::<MtTouch>(), 8);
            assert_eq!(offset_of!(MtTouch, timestamp), 8);
            assert_eq!(offset_of!(MtTouch, normalized_position), 32);
            assert_eq!(offset_of!(MtTouch, reserved_field9), 52);
            assert_eq!(offset_of!(MtTouch, absolute_position), 68);
            assert_eq!(offset_of!(MtTouch, z_density), 92);
        }

        #[test]
        fn contact_state_emits_with_every_force_related_field_at_zero() {
            let (sender, receiver) = mpsc::sync_channel(1);
            let dropped = Arc::new(AtomicU64::new(0));
            let mut state = CallbackState {
                reducer: ContactReducer::new(),
                sender,
                dropped: Arc::clone(&dropped),
                frames_seen: Arc::new(AtomicU64::new(0)),
                contacts_seen: Arc::new(AtomicU64::new(0)),
                touch_states_seen: Arc::new(AtomicU64::new(0)),
            };
            state.reducer.set_target(Some(ControllerSlot::One));
            state.process(
                &[MtTouch {
                    frame: 1,
                    timestamp: 0.0,
                    identifier: 7,
                    state: 3,
                    finger_id: 0,
                    hand_id: 0,
                    normalized_position: MtVector {
                        position: MtPoint { x: 0.6, y: 0.5 },
                        velocity: MtPoint { x: 0.0, y: 0.0 },
                    },
                    z_total: 0.0,
                    reserved_field9: 0,
                    angle: 0.0,
                    major_axis: 0.0,
                    minor_axis: 0.0,
                    absolute_position: MtVector {
                        position: MtPoint { x: 0.0, y: 0.0 },
                        velocity: MtPoint { x: 0.0, y: 0.0 },
                    },
                    field14: 0,
                    field15: 0,
                    z_density: 0.0,
                }],
                Instant::now(),
            );

            let hit = receiver.try_recv().expect("contact-only hit");
            assert_eq!(hit.slot, ControllerSlot::One);
            assert_eq!(hit.action, TaikoAction::RIGHT_DON);
            assert_eq!(dropped.load(std::sync::atomic::Ordering::Relaxed), 0);
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use anyhow::{bail, Result};

    use super::MacTrackpadDrain;
    use crate::controller::ControllerSlot;

    pub(super) struct Backend;

    impl Backend {
        pub(super) fn open() -> Result<Self> {
            bail!("native contact input requires macOS")
        }

        pub(super) fn set_target(&mut self, _target: Option<ControllerSlot>) -> Result<()> {
            Ok(())
        }

        pub(super) fn drain(&mut self) -> Result<MacTrackpadDrain> {
            Ok(MacTrackpadDrain::default())
        }

        pub(super) fn shutdown(&mut self) -> Result<()> {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use rhythm_mode_taiko::TaikoAction;

    use super::{action_for_normalized_x, ContactReducer, TrackpadContact, MAX_CONTACTS_PER_FRAME};
    use crate::controller::ControllerSlot;

    fn contact(identity: i32, normalized_x: f32) -> TrackpadContact {
        TrackpadContact {
            identity,
            normalized_x,
            touching: true,
        }
    }

    fn actions(reducer: &mut ContactReducer, contacts: &[TrackpadContact]) -> Vec<TaikoAction> {
        reducer
            .process_frame(contacts)
            .iter()
            .map(|hit| hit.action)
            .collect()
    }

    #[test]
    fn normalized_trackpad_quarters_map_to_the_four_physical_pads() {
        assert_eq!(action_for_normalized_x(0.0), Some(TaikoAction::LEFT_KAT));
        assert_eq!(
            action_for_normalized_x(0.249_999),
            Some(TaikoAction::LEFT_KAT)
        );
        assert_eq!(action_for_normalized_x(0.25), Some(TaikoAction::LEFT_DON));
        assert_eq!(action_for_normalized_x(0.5), Some(TaikoAction::RIGHT_DON));
        assert_eq!(action_for_normalized_x(0.75), Some(TaikoAction::RIGHT_KAT));
        assert_eq!(action_for_normalized_x(1.0), Some(TaikoAction::RIGHT_KAT));
        assert_eq!(action_for_normalized_x(f32::NAN), None);
    }

    #[test]
    fn one_contact_emits_once_until_it_is_lifted() {
        let mut reducer = ContactReducer::new();
        reducer.set_target(Some(ControllerSlot::One));

        assert_eq!(
            actions(&mut reducer, &[contact(7, 0.4)]),
            [TaikoAction::LEFT_DON]
        );
        assert!(actions(&mut reducer, &[contact(7, 0.4)]).is_empty());
        assert!(actions(&mut reducer, &[contact(7, 0.9)]).is_empty());
        assert!(actions(&mut reducer, &[]).is_empty());
        assert_eq!(
            actions(&mut reducer, &[contact(7, 0.9)]),
            [TaikoAction::RIGHT_KAT]
        );
    }

    #[test]
    fn simultaneous_new_contacts_preserve_all_four_hits() {
        let mut reducer = ContactReducer::new();
        reducer.set_target(Some(ControllerSlot::Two));

        assert_eq!(
            actions(
                &mut reducer,
                &[
                    contact(1, 0.1),
                    contact(2, 0.3),
                    contact(3, 0.6),
                    contact(4, 0.9),
                ]
            ),
            [
                TaikoAction::LEFT_KAT,
                TaikoAction::LEFT_DON,
                TaikoAction::RIGHT_DON,
                TaikoAction::RIGHT_KAT,
            ]
        );
    }

    #[test]
    fn changing_player_while_touching_waits_for_a_neutral_frame() {
        let mut reducer = ContactReducer::new();
        reducer.set_target(Some(ControllerSlot::One));
        assert_eq!(
            actions(&mut reducer, &[contact(1, 0.1)]),
            [TaikoAction::LEFT_KAT]
        );

        reducer.set_target(Some(ControllerSlot::Two));
        assert!(actions(&mut reducer, &[contact(1, 0.1)]).is_empty());
        assert!(actions(&mut reducer, &[contact(1, 0.1), contact(2, 0.4)]).is_empty());
        assert!(actions(&mut reducer, &[]).is_empty());
        assert_eq!(
            actions(&mut reducer, &[contact(3, 0.6)]),
            [TaikoAction::RIGHT_DON]
        );
    }

    #[test]
    fn disabled_frames_track_contacts_without_emitting_or_rearming_held_fingers() {
        let mut reducer = ContactReducer::new();
        assert!(actions(&mut reducer, &[contact(1, 0.1)]).is_empty());
        reducer.set_target(Some(ControllerSlot::One));
        assert!(actions(&mut reducer, &[contact(1, 0.1)]).is_empty());
        assert!(actions(&mut reducer, &[]).is_empty());
        assert_eq!(
            actions(&mut reducer, &[contact(2, 0.3)]),
            [TaikoAction::LEFT_DON]
        );
    }

    #[test]
    fn invalidated_oversized_frame_requires_neutral_before_resuming() {
        let mut reducer = ContactReducer::new();
        reducer.set_target(Some(ControllerSlot::One));
        reducer.invalidate_frame();
        assert!(actions(&mut reducer, &[contact(1, 0.1)]).is_empty());
        assert!(actions(&mut reducer, &[]).is_empty());
        assert_eq!(
            actions(&mut reducer, &[contact(2, 0.9)]),
            [TaikoAction::RIGHT_KAT]
        );
        assert_eq!(MAX_CONTACTS_PER_FRAME, 32);
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires a physical macOS multitouch device"]
    fn physical_driver_repeatedly_opens_starts_and_shuts_down() {
        use super::{MacTrackpad, MacTrackpadAvailability};

        for _ in 0..8 {
            let mut trackpad = MacTrackpad::open();
            assert_eq!(trackpad.availability(), MacTrackpadAvailability::Available);
            trackpad
                .set_target(Some(ControllerSlot::One))
                .expect("enable physical trackpad target");
            trackpad.shutdown().expect("shutdown physical trackpad");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires a person to touch the physical trackpad within twenty seconds"]
    fn physical_contact_arrives_without_a_click_or_pressure_threshold() {
        use std::thread;
        use std::time::{Duration, Instant};

        use super::{MacTrackpad, MacTrackpadAvailability};

        let mut trackpad = MacTrackpad::open();
        assert_eq!(trackpad.availability(), MacTrackpadAvailability::Available);
        trackpad
            .set_target(Some(ControllerSlot::One))
            .expect("enable physical trackpad target");

        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if !trackpad
                .drain()
                .expect("drain physical trackpad")
                .hits
                .is_empty()
            {
                trackpad.shutdown().expect("shutdown physical trackpad");
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
        let (frames, contacts, states) = trackpad.diagnostics();
        panic!(
            "no new trackpad contact arrived within twenty seconds \
             (frames={frames}, contacts={contacts}, state_bits={states:#x})"
        );
    }
}
