use std::time::Duration;

use godot::{classes::Engine, prelude::*};
use tokio::{
    runtime::{self, Runtime},
    task::JoinHandle,
};

const ALPN: &[u8] = b"godot-iroh/0.1";

/// mDNS service under which servers advertise themselves on the local network.
const LAN_SERVICE: &str = "godot-iroh";

mod browser;
mod client;
mod connection;
mod server;

/// Local network address lookup (mDNS): a connection string then resolves on a LAN without
/// internet access. Servers `advertise`, clients and browsers only listen.
fn lan_lookup(advertise: bool) -> iroh_mdns_address_lookup::MdnsAddressLookupBuilder {
    iroh_mdns_address_lookup::MdnsAddressLookup::builder()
        .service_name(LAN_SERVICE)
        .advertise(advertise)
}

/// Android: iroh reads the network configuration (interfaces, DNS servers) through JNI, which
/// needs the JavaVM and the application context (`ndk_context`), else it panics. Godot loads a
/// GDExtension with `dlopen`, so this only runs once the game calls
/// `System.loadLibrary("godot_iroh_<arch>")` on the already loaded library.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn JNI_OnLoad(vm: jni::JavaVM, _: *mut std::ffi::c_void) -> jni::sys::jint {
    let app = vm.get_env().and_then(|mut env| {
        let app = env
            .call_static_method(
                "android/app/ActivityThread",
                "currentApplication",
                "()Landroid/app/Application;",
                &[],
            )?
            .l()?;
        env.new_global_ref(app)
    });
    match app {
        Ok(app) => {
            // SAFETY: the VM outlives the library, and the global ref is leaked so the context
            // stays valid until the process exits
            unsafe {
                iroh::dns::install_android_jni_context(
                    vm.get_java_vm_pointer().cast(),
                    app.as_obj().as_raw().cast(),
                );
            }
            std::mem::forget(app);
        }
        Err(e) => godot_error!("godot-iroh: no Android context for JNI: {e}"),
    }
    jni::sys::JNI_VERSION_1_6
}

struct MyExtension;

#[gdextension]
unsafe impl ExtensionLibrary for MyExtension {
    fn on_level_init(level: InitLevel) {
        if level == InitLevel::Scene {
            Engine::singleton().register_singleton("IrohRuntime", &IrohRuntime::new_alloc());
        }
    }

    fn on_level_deinit(level: InitLevel) {
        if level == InitLevel::Scene {
            let mut engine = Engine::singleton();
            let singleton = engine
                .get_singleton("IrohRuntime")
                .expect("singleton not found");
            engine.unregister_singleton("IrohRuntime");
            singleton.free();
        }
    }
}

#[derive(GodotClass)]
#[class(base=Object)]
pub struct IrohRuntime {
    base: Base<Object>,
    runtime: Option<Runtime>,
}

#[godot_api]
impl IObject for IrohRuntime {
    fn init(base: Base<Object>) -> Self {
        let runtime = Some(
            runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap(),
        );
        Self { base, runtime }
    }
}

#[godot_api]
impl IrohRuntime {
    pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        Engine::singleton()
            .get_singleton("IrohRuntime")
            .expect("singleton not found")
            .cast::<Self>()
            .bind()
            .runtime
            .as_ref()
            .expect("invalid singleton")
            .spawn(future)
    }

    pub fn block_on<F: Future>(future: F) -> F::Output {
        Engine::singleton()
            .get_singleton("IrohRuntime")
            .expect("singleton not found")
            .cast::<Self>()
            .bind()
            .runtime
            .as_ref()
            .expect("invalid singleton")
            .block_on(future)
    }
}

impl Drop for IrohRuntime {
    fn drop(&mut self) {
        if let Some(runtime) = std::mem::take(&mut self.runtime) {
            runtime.shutdown_timeout(Duration::from_secs(5));
        }
    }
}
