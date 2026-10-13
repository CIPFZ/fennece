//! Service control manager integration (`windows-service`).

use std::ffi::OsString;
use std::sync::OnceLock;
use std::time::Duration;

use fnc_platform::StopFlag;
use windows_service::service::{
    ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode, ServiceInfo,
    ServiceStartType, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_dispatcher;
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

/// Service name for an instance (`FennecService`, `FennecService_work`).
pub fn service_name(instance: &str) -> String {
    if instance.is_empty() { "FennecService".into() } else { format!("FennecService_{instance}") }
}

static INSTANCE: OnceLock<String> = OnceLock::new();

// The macro generates the `extern "system"` entry point the service manager calls; its
// only unsafe operation is converting the argument vector (inside `windows-service`).
windows_service::define_windows_service!(ffi_service_main, service_main);

/// Hands the process to the service control dispatcher (blocks while running).
pub fn dispatch(instance: &str) -> Result<(), String> {
    let _ = INSTANCE.set(instance.to_string());
    service_dispatcher::start(service_name(instance), ffi_service_main).map_err(|e| {
        format!("{e} (start it with `sc start {}` or run `fennec-service -console`)", service_name(instance))
    })
}

fn service_main(_args: Vec<OsString>) {
    if let Err(e) = run_service() {
        tracing::error!(error = %e, "service failed");
    }
}

fn run_service() -> windows_service::Result<()> {
    let instance = INSTANCE.get().cloned().unwrap_or_default();
    let stop = StopFlag::new();
    let s = stop.clone();
    let inst = instance.clone();
    let handler = move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            s.stop();
            super::server::wake(&inst);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let status = service_control_handler::register(service_name(&instance), handler)?;
    let set = |state, accept| {
        status.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: accept,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::from_secs(5),
            process_id: None,
        })
    };
    set(ServiceState::Running, ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN)?;
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "service started");
    if let Err(e) = super::server::serve(&instance, &stop) {
        tracing::error!(error = %e, "pipe server failed");
    }
    tracing::info!("service stopping");
    set(ServiceState::Stopped, ServiceControlAccept::empty())
}

/// Registers the service (LocalSystem, automatic start) and starts it.
pub fn install(instance: &str) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )
    .map_err(|e| format!("cannot open the service manager (run elevated): {e}"))?;
    let mut launch_arguments = Vec::new();
    if !instance.is_empty() {
        launch_arguments.push(OsString::from("-instance"));
        launch_arguments.push(OsString::from(instance));
    }
    let info = ServiceInfo {
        name: service_name(instance).into(),
        display_name: format!(
            "Fennec Service{}",
            if instance.is_empty() { String::new() } else { format!(" ({instance})") }
        )
        .into(),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: exe,
        launch_arguments,
        dependencies: vec![],
        account_name: None, // LocalSystem
        account_password: None,
    };
    let service = match manager.create_service(&info, ServiceAccess::START | ServiceAccess::CHANGE_CONFIG) {
        Ok(s) => s,
        Err(_) => manager
            .open_service(service_name(instance), ServiceAccess::START | ServiceAccess::CHANGE_CONFIG)
            .and_then(|s| s.change_config(&info).map(|()| s))
            .map_err(|e| format!("cannot create the service: {e}"))?,
    };
    let _ = service.set_description("Lets Fennec index NTFS volumes without administrator rights.");
    if let Err(e) = service.start::<&str>(&[]) {
        // Already running is fine.
        tracing::info!(error = %e, "start after install");
    }
    println!("{} installed and started", service_name(instance));
    Ok(())
}

/// Stops and deletes the service.
pub fn uninstall(instance: &str) -> Result<(), String> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|e| format!("cannot open the service manager (run elevated): {e}"))?;
    let service = manager
        .open_service(service_name(instance), ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE)
        .map_err(|e| format!("{} is not installed: {e}", service_name(instance)))?;
    if let Ok(st) = service.query_status()
        && st.current_state != ServiceState::Stopped
    {
        let _ = service.stop();
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(100));
            if service.query_status().is_ok_and(|s| s.current_state == ServiceState::Stopped) {
                break;
            }
        }
    }
    service.delete().map_err(|e| format!("cannot delete the service: {e}"))?;
    println!("{} removed", service_name(instance));
    Ok(())
}
