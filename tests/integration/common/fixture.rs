use std::{
    os::fd::AsFd,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use pinnacle::state::{ClientState, Pinnacle};
use smithay::{
    output::Output,
    reexports::calloop::{EventLoop, Interest, Mode, PostAction, generic::Generic},
    utils::{Logical, Rectangle, Transform},
};
use tracing::debug;
use wayland_client::protocol::wl_surface::WlSurface;

use super::{
    client::{Client, ClientId, Window},
    server::Server,
};

static TEST_MUTEX: Mutex<()> = Mutex::new(());

pub struct Fixture {
    event_loop: EventLoop<'static, State>,
    state: State,
    _test_guard: MutexGuard<'static, ()>,
    timeout: Duration,
}

struct State {
    server: Server,
    clients: Vec<Client>,
}

static OUTPUT_COUNTER: AtomicU32 = AtomicU32::new(0);
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

impl Fixture {
    pub fn new() -> Self {
        Self::new_inner(false)
    }

    pub fn new_with_socket() -> Self {
        Self::new_inner(true)
    }

    pub fn new_inner(create_socket: bool) -> Self {
        let _test_guard = TEST_MUTEX.lock().unwrap_or_else(|guard| {
            TEST_MUTEX.clear_poison();
            guard.into_inner()
        });

        let state = State {
            server: Server::new(create_socket),
            clients: Vec::new(),
        };

        let event_loop = EventLoop::try_new().unwrap();

        // Fold the server's event loop into the fixture's
        let fd = state
            .server
            .event_loop
            .as_fd()
            .try_clone_to_owned()
            .unwrap();
        let source = Generic::new(fd, Interest::READ, Mode::Level);
        event_loop
            .handle()
            .insert_source(source, |_, _, state: &mut State| {
                state.server.dispatch();
                Ok(PostAction::Continue)
            })
            .unwrap();

        Self {
            event_loop,
            state,
            _test_guard,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    pub fn runtime_handle(&self) -> tokio::runtime::Handle {
        self.state.server.runtime.handle().clone()
    }

    pub fn add_client(&mut self) -> ClientId {
        let (sock1, sock2) = std::os::unix::net::UnixStream::pair().unwrap();

        let client = Client::new(sock2);
        let id = client.id();

        // Fold the client's event loop into the fixture's
        self.pinnacle()
            .display_handle
            .insert_client(sock1, Arc::new(ClientState::default()))
            .unwrap();
        let fd = client.event_loop_fd();
        let source = Generic::new(fd, Interest::READ, Mode::Level);
        self.event_loop
            .handle()
            .insert_source(source, move |_, _, state: &mut State| {
                state.client(id).dispatch();
                Ok(PostAction::Continue)
            })
            .unwrap();

        self.state.clients.push(client);
        self.roundtrip(id);
        id
    }

    pub fn add_output(&mut self, geo: Rectangle<i32, Logical>) -> Output {
        let name = format!(
            "pinnacle-{}",
            OUTPUT_COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        self.pinnacle().new_output(
            name,
            "",
            "",
            geo.loc,
            geo.size.to_physical(1),
            60000,
            1.0,
            Transform::Normal,
        )
    }

    pub fn state(&mut self) -> &mut pinnacle::state::State {
        &mut self.state.server.state
    }

    pub fn pinnacle(&mut self) -> &mut Pinnacle {
        &mut self.state().pinnacle
    }

    pub fn dispatch(&mut self) {
        self.event_loop
            .dispatch(Duration::ZERO, &mut self.state)
            .unwrap();
    }

    pub fn dispatch_until<F>(&mut self, mut until: F)
    where
        F: FnMut(&mut Self) -> bool,
    {
        let start = std::time::Instant::now();

        while !until(self) {
            self.dispatch();

            if start.elapsed() > self.timeout {
                panic!("Timeout reached");
            }
        }
    }

    pub fn dispatch_for(&mut self, duration: Duration) {
        let start = std::time::Instant::now();

        while start.elapsed() <= duration {
            self.dispatch();
        }
    }

    /// Spawns a blocking API call and dispatches the event loop until it is finished.
    #[track_caller]
    pub fn spawn_blocking<F, T>(&mut self, spawn: F) -> T
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let handle = self.runtime_handle();
        let _guard = handle.enter();
        let join = handle.spawn_blocking(spawn);
        self.dispatch_until(|_| join.is_finished());

        match self.runtime_handle().block_on(join) {
            Ok(ret) => ret,
            Err(err) => {
                panic!("rust panicked: {err}");
            }
        }
    }

    pub fn roundtrip(&mut self, id: ClientId) {
        let client = self.client(id);
        let wait = client.send_sync();
        while !wait.load(Ordering::Relaxed) {
            self.dispatch();
        }
        debug!(client = ?id, "roundtripped");
    }

    // TODO: Remove this once the tests are confirmed to be stable
    #[allow(dead_code)]
    pub fn double_roundtrip(&mut self, id: ClientId) {
        self.roundtrip(id);
        self.roundtrip(id);
    }

    pub fn spawn_window_with<F>(&mut self, id: ClientId, mut pre_initial_commit: F) -> WlSurface
    where
        F: FnMut(&mut Window),
    {
        let old_trees = self.pinnacle().layout_state.layout_trees.clone();

        // Add a window
        let window = self.client(id).create_window();
        pre_initial_commit(window);
        window.commit();
        let surface = window.surface();
        self.roundtrip(id);

        // Commit a buffer
        let window = self.client(id).window_for_surface(&surface);
        window.attach_buffer();
        let current_serial = window.current_serial();
        assert!(current_serial.is_some());
        window.ack_and_commit();
        assert!(window.current_serial().is_none());
        self.roundtrip(id);

        // Let Pinnacle do a layout
        self.dispatch_until(|fixture| fixture.pinnacle().layout_state.layout_trees != old_trees);
        self.roundtrip(id);

        // Commit the layout
        self.wait_client_configure(id);
        self.client(id).ack_all_window();
        self.roundtrip(id);

        // Waiting one last time because we're getting focused/activated at this point.
        self.wait_client_configure(id);
        self.client(id).ack_all_window();
        self.roundtrip(id);

        // Wait for pending_transactions, if any
        self.flush();

        surface
    }

    pub fn spawn_floating_window_with<F>(
        &mut self,
        id: ClientId,
        size: (i32, i32),
        mut pre_initial_commit: F,
    ) -> WlSurface
    where
        F: FnMut(&mut Window),
    {
        // Add a window
        let window = self.client(id).create_window();
        window.set_min_size(size.0, size.1);
        window.set_max_size(size.0, size.1);
        pre_initial_commit(window);

        tracing::debug!("Sending initial commit");
        window.commit();
        let surface = window.surface();
        self.roundtrip(id);

        tracing::debug!("Wait for initial_commit");
        // The client must acknowledge [the initial configure] before committing a buffer. Let's
        // ensure it does.
        self.wait_client_configure(id);

        // Commit a buffer
        let window = self.client(id).window_for_surface(&surface);
        window.attach_buffer();
        window.set_size(size.0, size.1);
        tracing::debug!("Buffer & size commit");
        window.commit();
        self.roundtrip(id);

        tracing::debug!("Wait for size configuration");
        // wait a bit for the size to be set
        self.wait_client_configure(id);
        self.client(id).ack_all_window();
        self.roundtrip(id);

        // Waiting one last time because we're getting focused/activated at this point.
        let pinnacle_win = self.pinnacle().windows.last().cloned().unwrap();
        let output = pinnacle_win.output(self.pinnacle());
        let focused_output = self.pinnacle().focused_output();

        if output.as_ref() == focused_output {
            tracing::info!("Wait activation");
            // Waiting to be activated/focused.
            self.wait_client_configure(id);
            self.client(id).ack_all_window();
            self.roundtrip(id);
        }

        // Wait for pending_transactions, if any
        self.flush();

        surface
    }

    pub fn spawn_windows(&mut self, amount: u8, id: ClientId) -> Vec<WlSurface> {
        let surfaces = (0..amount)
            .map(|_| self.spawn_window_with(id, |_| ()))
            .collect::<Vec<_>>();

        surfaces
    }

    pub fn client(&mut self, id: ClientId) -> &mut Client {
        self.state.client(id)
    }

    /// Wait for all outstanding transaction and configure to be handled.
    ///
    /// If timeout is reached, stop with false.
    pub fn flush(&mut self) {
        let start = std::time::Instant::now();
        let mut loop_again = true;

        let client_ids = self
            .state
            .clients
            .iter()
            .map(|c| c.id())
            .collect::<Vec<_>>();

        while loop_again {
            tracing::debug!("Flushing transaction and configure");
            for id in client_ids.iter().cloned() {
                loop_again |= self.client(id).ack_all_window();
                self.roundtrip(id);
            }
            self.dispatch();

            loop_again = !self.pinnacle().layout_state.pending_transactions.is_empty();

            if start.elapsed() >= self.timeout {
                panic!("Timeout reached");
            }
        }
    }

    /// Wait for a client to receive a configure, dispatching in the meantime.
    pub fn wait_client_configure(&mut self, id: ClientId) {
        let start = std::time::Instant::now();

        while !self.client(id).has_pending_configure() {
            if start.elapsed() >= self.timeout {
                panic!("Timeout reached");
            }

            self.dispatch();
        }
    }
}

impl State {
    pub fn client(&mut self, id: ClientId) -> &mut Client {
        self.clients
            .iter_mut()
            .find(|client| client.id() == id)
            .unwrap()
    }
}

#[macro_export]
macro_rules! spawn_lua_blocking {
    ($fixture:expr, $($code:tt)*) => {{
        let join = ::std::thread::spawn(move || {
            let lua = $crate::common::new_lua();
            let task = lua.load(::mlua::chunk! {
                Pinnacle.run(function()
                    local run = function()
                        $($code)*
                    end

                    local success, err = pcall(run)

                    if not success then
                        error(err)
                    end
                end)
            });

            if let Err(err) = task.exec() {
                panic!("lua panicked: {err}");
            }
        });

        $fixture.dispatch_until(|_| join.is_finished());
        join.join().unwrap();
    }};
}
