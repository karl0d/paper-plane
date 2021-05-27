use crate::{Application, RUNTIME};
use crate::config::{APP_ID, PROFILE};
use glib::{SyncSender, clone};
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::glib;
use gtk_macros::send;
use log::error;
use tokio::task;
use std::sync::Arc;
use tdgrand::{
    enums::{AuthorizationState, Update},
    functions,
};

mod imp {
    use super::*;
    use crate::Login;
    use adw::subclass::prelude::AdwApplicationWindowImpl;
    use gtk::{CompositeTemplate, Inhibit, gio};
    use log::warn;
    use std::cell::RefCell;
    use tokio::task::JoinHandle;

    #[derive(Debug, CompositeTemplate)]
    #[template(resource = "/com/github/melix99/telegrand/ui/window.ui")]
    pub struct Window {
        #[template_child]
        pub login: TemplateChild<Login>,
        pub settings: gio::Settings,
        pub receiver_handle: RefCell<Option<JoinHandle<()>>>,
        pub client_id: i32,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Window {
        const NAME: &'static str = "Window";
        type Type = super::Window;
        type ParentType = adw::ApplicationWindow;

        fn new() -> Self {
            Self {
                login: TemplateChild::default(),
                settings: gio::Settings::new(APP_ID),
                receiver_handle: RefCell::default(),
                client_id: tdgrand::crate_client(),
            }
        }

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for Window {
        fn constructed(&self, obj: &Self::Type) {
            self.parent_constructed(obj);

            let builder =
                gtk::Builder::from_resource("/com/github/melix99/telegrand/ui/shortcuts.ui");
            let shortcuts = builder.object("shortcuts").unwrap();
            obj.set_help_overlay(Some(&shortcuts));

            // Devel profile
            if PROFILE == "Devel" {
                obj.style_context().add_class("devel");
            }

            // Load latest window state
            obj.load_window_size();

            // Start the receiver for telegram responses and updates
            obj.start_td_receiver();

            obj.login_client();
        }
    }

    impl WidgetImpl for Window {}
    impl WindowImpl for Window {
        // Save window state on delete event
        fn close_request(&self, obj: &Self::Type) -> Inhibit {
            // Send close request
            obj.close_client();

            // Await for the td receiver to end
            obj.await_td_receiver();

            if let Err(err) = obj.save_window_size() {
                warn!("Failed to save window state, {}", &err);
            }

            Inhibit(false)
        }
    }

    impl ApplicationWindowImpl for Window {}
    impl AdwApplicationWindowImpl for Window {}
}

glib::wrapper! {
    pub struct Window(ObjectSubclass<imp::Window>)
        @extends gtk::Widget, gtk::Window, gtk::ApplicationWindow, adw::ApplicationWindow;
}

impl Window {
    pub fn new(app: &Application) -> Self {
        glib::Object::new(&[("application", &app), ("icon-name", &APP_ID)])
            .expect("Failed to create Window")
    }

    pub fn save_window_size(&self) -> Result<(), glib::BoolError> {
        let settings = &imp::Window::from_instance(self).settings;

        let size = self.default_size();
        settings.set_int("window-width", size.0)?;
        settings.set_int("window-height", size.1)?;

        settings.set_boolean("is-maximized", self.is_maximized())?;

        Ok(())
    }

    fn load_window_size(&self) {
        let settings = &imp::Window::from_instance(self).settings;

        let width = settings.int("window-width");
        let height = settings.int("window-height");
        self.set_default_size(width, height);

        let is_maximized = settings.boolean("is-maximized");
        if is_maximized {
            self.maximize();
        }
    }

    fn start_td_receiver(&self) {
        let priv_ = imp::Window::from_instance(self);
        let sender = Arc::new(self.create_new_update_sender());
        let handle = RUNTIME.spawn(async move {
            loop {
                let sender = sender.clone();
                let stop = task::spawn_blocking(move || {
                    if let Some((update, _)) = tdgrand::receive() {
                        if let Update::AuthorizationState(update) = &update {
                            if let AuthorizationState::Closed = update.authorization_state {
                                return true;
                            }
                        }

                        send!(sender, update);
                    }

                    false
                }).await.unwrap();

                if stop {
                    break;
                }
            }
        });

        priv_.receiver_handle.replace(Some(handle));
    }

    fn await_td_receiver(&self) {
        let receiver_handle = &imp::Window::from_instance(self).receiver_handle;
        RUNTIME.block_on(async move {
            receiver_handle.borrow_mut().as_mut().unwrap().await.unwrap();
        });
    }

    fn close_client(&self) {
        let client_id = imp::Window::from_instance(self).client_id;
        RUNTIME.spawn(async move {
            functions::close(client_id).await.unwrap();
        });
    }

    fn login_client(&self) {
        let priv_ = imp::Window::from_instance(self);
        let client_id = priv_.client_id;
        let login = &priv_.login;

        login.set_client_id(client_id);

        // This call is important for login because TDLib requires the clients
        // to do at least a request to start receiving updates. So with this
        // call we both set our preferred log level and we also enable the
        // client to receive updates.
        RUNTIME.spawn(async move {
            functions::set_log_verbosity_level(client_id, 2).await.unwrap();
        });
    }

    fn create_new_update_sender(&self) -> SyncSender<Update> {
        let (sender, receiver) =
            glib::MainContext::sync_channel::<Update>(Default::default(), 100);
        receiver.attach(
            None,
            clone!(@weak self as obj => @default-return glib::Continue(false), move |update| {
                obj.handle_update(update);

                glib::Continue(true)
            }),
        );

        sender
    }

    fn handle_update(&self, update: Update) {
        if let Update::AuthorizationState(update) = update {
            let login = &imp::Window::from_instance(self).login;
            login.set_authorization_state(update.authorization_state);
        }
    }
}
