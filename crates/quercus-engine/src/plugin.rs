//! C ABI for the extension-plugin protocol (prefix `quercus_plugin_`).
//!
//! A plugin registers a descriptor (id/name/version/author/description)
//! plus one command callback, then contributes commands (menu items).
//! The host enumerates plugins and their commands to build menus;
//! invoking a command calls back into the owning plugin.
//!
//! The registry is process-global. Command callbacks run WITHOUT the
//! registry lock held, so a callback may call back into the registry
//! (e.g. add another command).

use std::ffi::{c_char, c_void, CString};
use std::sync::Mutex;

use crate::codec::{copy_str_out, str_arg};

/// A plugin's static metadata. Fixed-size NUL-terminated fields.
#[repr(C)]
pub struct QuercusPluginDescriptor {
	/// Unique reverse-DNS id (e.g. "com.example.myplugin").
	pub id: [u8; 128],
	/// Display name.
	pub name: [u8; 256],
	/// Version string.
	pub version: [u8; 64],
	/// Author.
	pub author: [u8; 256],
	/// Short description.
	pub description: [u8; 1024],
}

/// The command callback: the host invokes it with the command id. The
/// `command_id` pointer is valid for the call duration only.
pub type QuercusPluginCommandFn =
	unsafe extern "C" fn(command_id: *const c_char, userdata: *mut c_void);

struct PluginCommand {
	id: String,
	title: String,
	menu_path: String,
}

struct PluginEntry {
	id: String,
	name: String,
	version: String,
	author: String,
	description: String,
	on_command: QuercusPluginCommandFn,
	userdata: *mut c_void,
	commands: Vec<PluginCommand>,
}

// The userdata pointer is opaque host state; registry access is
// mutex-guarded and callbacks run outside the lock.
unsafe impl Send for PluginEntry {}

static PLUGINS: Mutex<Vec<PluginEntry>> = Mutex::new(Vec::new());

fn read_cstr(field: &[u8]) -> String {
	let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
	String::from_utf8_lossy(&field[..end]).into_owned()
}

/// Registers a plugin. Returns its slot index, -1 on null input, an
/// empty/duplicate id, or a missing callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_plugin_register(
	descriptor: *const QuercusPluginDescriptor,
	on_command: Option<QuercusPluginCommandFn>,
	userdata: *mut c_void,
) -> i32 {
	if descriptor.is_null() {
		return -1;
	}
	let Some(on_command) = on_command else {
		return -1;
	};
	let d = unsafe { &*descriptor };
	let id = read_cstr(&d.id);
	if id.is_empty() {
		return -1;
	}
	let mut registry = PLUGINS.lock().unwrap_or_else(|e| e.into_inner());
	if registry.iter().any(|p| p.id == id) {
		return -1;
	}
	registry.push(PluginEntry {
		id,
		name: read_cstr(&d.name),
		version: read_cstr(&d.version),
		author: read_cstr(&d.author),
		description: read_cstr(&d.description),
		on_command,
		userdata,
		commands: Vec::new(),
	});
	(registry.len() - 1) as i32
}

/// Unregisters the plugin at `index`, dropping its commands. False on
/// out-of-range index.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_plugin_unregister(index: i32) -> bool {
	let mut registry = PLUGINS.lock().unwrap_or_else(|e| e.into_inner());
	if index < 0 || index as usize >= registry.len() {
		return false;
	}
	registry.remove(index as usize);
	true
}

/// Number of registered plugins.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_plugin_count() -> i32 {
	PLUGINS.lock().unwrap_or_else(|e| e.into_inner()).len() as i32
}

/// Copies the descriptor of the plugin at `index`. False when out of
/// range.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_plugin_descriptor(
	index: i32,
	out: *mut QuercusPluginDescriptor,
) -> bool {
	if out.is_null() {
		return false;
	}
	let registry = PLUGINS.lock().unwrap_or_else(|e| e.into_inner());
	if index < 0 || index as usize >= registry.len() {
		return false;
	}
	let p = &registry[index as usize];
	let write = |field: &mut [u8], value: &str| {
		field.fill(0);
		let n = value.len().min(field.len() - 1);
		field[..n].copy_from_slice(&value.as_bytes()[..n]);
	};
	let out = unsafe { &mut *out };
	write(&mut out.id, &p.id);
	write(&mut out.name, &p.name);
	write(&mut out.version, &p.version);
	write(&mut out.author, &p.author);
	write(&mut out.description, &p.description);
	true
}

/// Contributes a command (menu item) to the plugin at `index`.
/// `menu_path` uses `/` separators (e.g. "Tools/Export"). False on
/// invalid input or out-of-range index.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_plugin_add_command(
	index: i32,
	command_id: *const u8,
	command_id_len: usize,
	title: *const u8,
	title_len: usize,
	menu_path: *const u8,
	menu_path_len: usize,
) -> bool {
	let (Some(id), Some(title), Some(menu_path)) = (
		unsafe { str_arg(command_id, command_id_len) },
		unsafe { str_arg(title, title_len) },
		unsafe { str_arg(menu_path, menu_path_len) },
	) else {
		return false;
	};
	if id.is_empty() {
		return false;
	}
	let mut registry = PLUGINS.lock().unwrap_or_else(|e| e.into_inner());
	if index < 0 || index as usize >= registry.len() {
		return false;
	}
	let plugin = &mut registry[index as usize];
	if plugin.commands.iter().any(|c| c.id == id) {
		return false;
	}
	plugin.commands.push(PluginCommand {
		id: id.to_string(),
		title: title.to_string(),
		menu_path: menu_path.to_string(),
	});
	true
}

/// Number of commands the plugin at `index` contributes.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_plugin_command_count(index: i32) -> i32 {
	let registry = PLUGINS.lock().unwrap_or_else(|e| e.into_inner());
	if index < 0 || index as usize >= registry.len() {
		return 0;
	}
	registry[index as usize].commands.len() as i32
}

macro_rules! plugin_command_getter {
	($( $name:ident => $field:ident ),* $(,)?) => {
		$(
			#[doc = concat!("The command's `", stringify!($field), "`, snprintf-style; -1 when out of range.")]
			#[unsafe(no_mangle)]
			pub unsafe extern "C" fn $name(
				index: i32,
				command_index: i32,
				buf: *mut u8,
				buf_len: usize,
			) -> i32 {
				let registry = PLUGINS.lock().unwrap_or_else(|e| e.into_inner());
				if index < 0 || index as usize >= registry.len() {
					return -1;
				}
				let commands = &registry[index as usize].commands;
				if command_index < 0 || command_index as usize >= commands.len() {
					return -1;
				}
				copy_str_out(&commands[command_index as usize].$field, buf, buf_len)
			}
		)*
	};
}

plugin_command_getter! {
	quercus_plugin_command_id_at => id,
	quercus_plugin_command_title_at => title,
	quercus_plugin_command_menu_path_at => menu_path,
}

/// Invokes a command by id: finds the contributing plugin and calls its
/// command callback (with its userdata). False when no plugin
/// contributes it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_plugin_invoke_command(
	command_id: *const u8,
	command_id_len: usize,
) -> bool {
	let Some(id) = (unsafe { str_arg(command_id, command_id_len) }) else {
		return false;
	};
	// Copy out what the callback needs, then drop the lock: the callback
	// may call back into the registry.
	let target = {
		let registry = PLUGINS.lock().unwrap_or_else(|e| e.into_inner());
		registry
			.iter()
			.find(|p| p.commands.iter().any(|c| c.id == id))
			.map(|p| (p.on_command, p.userdata))
	};
	let Some((on_command, userdata)) = target else {
		return false;
	};
	let c_id = CString::new(id).unwrap_or_default();
	unsafe { on_command(c_id.as_ptr(), userdata) };
	true
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::Mutex;
	use std::sync::atomic::{AtomicUsize, Ordering};

	// The registry is process-global; serialize the plugin tests.
	static SERIAL: Mutex<()> = Mutex::new(());

	static CALLS: AtomicUsize = AtomicUsize::new(0);
	static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());

	unsafe extern "C" fn recording_cb(command_id: *const c_char, _userdata: *mut c_void) {
		CALLS.fetch_add(1, Ordering::SeqCst);
		let id = unsafe { std::ffi::CStr::from_ptr(command_id) }
			.to_string_lossy()
			.into_owned();
		SEEN.lock().unwrap().push(id);
	}

	fn descriptor(id: &str, name: &str) -> QuercusPluginDescriptor {
		fn fill<const N: usize>(field: &mut [u8; N], value: &str) {
			let n = value.len().min(N - 1);
			field[..n].copy_from_slice(&value.as_bytes()[..n]);
		}
		let mut d = QuercusPluginDescriptor {
			id: [0; 128],
			name: [0; 256],
			version: [0; 64],
			author: [0; 256],
			description: [0; 1024],
		};
		fill(&mut d.id, id);
		fill(&mut d.name, name);
		fill(&mut d.version, "0.1.0");
		d
	}

	#[test]
	fn register_contribute_invoke_unregister() {
		let _guard = SERIAL.lock().unwrap();
		unsafe {
			let id = "com.quercus.test.batch4";
			let base = quercus_plugin_count();

			let d = descriptor(id, "Batch4 Test");
			let index = quercus_plugin_register(&d, Some(recording_cb), std::ptr::null_mut());
			assert!(index >= 0);
			assert_eq!(quercus_plugin_count(), base + 1);

			// Duplicate ids are rejected.
			let d2 = descriptor(id, "Dup");
			assert_eq!(quercus_plugin_register(&d2, Some(recording_cb), std::ptr::null_mut()), -1);

			// Descriptor roundtrip.
			let mut out = descriptor("", "");
			assert!(quercus_plugin_descriptor(index, &mut out));
			assert_eq!(read_cstr(&out.id), id);
			assert_eq!(read_cstr(&out.name), "Batch4 Test");

			// Contribute a command and invoke it.
			let cid = "export_markers";
			assert!(quercus_plugin_add_command(
				index,
				cid.as_ptr(),
				cid.len(),
				"Export Markers".as_ptr(),
				14,
				"Tools".as_ptr(),
				5,
			));
			assert_eq!(quercus_plugin_command_count(index), 1);

			let mut buf = [0u8; 64];
			let n = quercus_plugin_command_id_at(index, 0, buf.as_mut_ptr(), buf.len());
			assert_eq!(std::str::from_utf8(&buf[..n as usize]).unwrap(), "export_markers");
			let n = quercus_plugin_command_menu_path_at(index, 0, buf.as_mut_ptr(), buf.len());
			assert_eq!(std::str::from_utf8(&buf[..n as usize]).unwrap(), "Tools");

			let calls_before = CALLS.load(Ordering::SeqCst);
			assert!(quercus_plugin_invoke_command(cid.as_ptr(), cid.len()));
			assert_eq!(SEEN.lock().unwrap().last().map(String::as_str), Some("export_markers"));
			assert_eq!(CALLS.load(Ordering::SeqCst), calls_before + 1);

			// Unknown command: not invoked.
			assert!(!quercus_plugin_invoke_command("nope".as_ptr(), 4));

			assert!(quercus_plugin_unregister(index));
			assert_eq!(quercus_plugin_count(), base);
			assert!(!quercus_plugin_unregister(index), "already gone");
		}
	}

	#[test]
	fn register_rejects_bad_input() {
		let _guard = SERIAL.lock().unwrap();
		unsafe {
			assert_eq!(
				quercus_plugin_register(std::ptr::null(), Some(recording_cb), std::ptr::null_mut()),
				-1
			);
			let d = descriptor("com.quercus.test.nocb", "No Callback");
			assert_eq!(quercus_plugin_register(&d, None, std::ptr::null_mut()), -1);
			let d = descriptor("", "Empty Id");
			assert_eq!(quercus_plugin_register(&d, Some(recording_cb), std::ptr::null_mut()), -1);
		}
	}
}
