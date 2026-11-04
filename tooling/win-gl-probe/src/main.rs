// Quercus Video Editor - Windows GL capability probe
// Copyright (C) 2026 Quercus Team
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Standalone, no Rust toolchain required on the test machine.
// It creates a hidden WGL context and prints the GL version/renderer plus
// whether the functions needed by OpenFX zero-copy are loadable:
//   GL_EXT_memory_object_win32
//   glCreateMemoryObjectsEXT / glImportMemoryWin32HandleEXT /
//   glTexStorageMem2DEXT

#![allow(non_snake_case)]
use std::ffi::{c_char, c_void, CStr};
use windows_sys::Win32::Foundation::HINSTANCE;
use windows_sys::Win32::Graphics::Gdi::{GetDC, ReleaseDC, HDC};
use windows_sys::Win32::Graphics::OpenGL::{
    wglCreateContext, wglDeleteContext, wglGetProcAddress, wglMakeCurrent, ChoosePixelFormat,
    SetPixelFormat, HGLRC, PIXELFORMATDESCRIPTOR, PFD_DOUBLEBUFFER, PFD_DRAW_TO_WINDOW,
    PFD_SUPPORT_OPENGL, PFD_TYPE_RGBA,
};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, UnregisterClassW, WNDCLASSW,
    CS_OWNDC, WS_OVERLAPPEDWINDOW,
};

type WglCreateContextAttribsArb = unsafe extern "system" fn(HDC, HGLRC, *const i32) -> HGLRC;
type GlGetString = unsafe extern "system" fn(u32) -> *const c_char;
type GlGetStringi = unsafe extern "system" fn(u32, u32) -> *const c_char;
type GlGetIntegerv = unsafe extern "system" fn(u32, *mut i32);

unsafe fn load_gl<T: Copy>(name: &[u8]) -> Option<T> {
    let p = unsafe { wglGetProcAddress(name.as_ptr()) };
    if let Some(f) = p {
        return Some(unsafe { std::mem::transmute_copy(&f) });
    }
    let module = unsafe { GetModuleHandleW(windows_sys::w!("opengl32.dll")) };
    if module.is_null() {
        return None;
    }
    unsafe { GetProcAddress(module, name.as_ptr()) }.map(|f| unsafe { std::mem::transmute_copy(&f) })
}

fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

fn main() {
    unsafe {
        let hinstance: HINSTANCE = GetModuleHandleW(std::ptr::null());
        let class_name = windows_sys::w!("QuercusProbeWGL");
        let wc = WNDCLASSW {
            style: CS_OWNDC,
            lpfnWndProc: Some(DefWindowProcW),
            hInstance: hinstance,
            lpszClassName: class_name,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            0,
            class_name,
            windows_sys::w!("probe"),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            8,
            8,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            println!("CreateWindow failed");
            return;
        }
        let hdc = GetDC(hwnd);
        let mut pfd: PIXELFORMATDESCRIPTOR = std::mem::zeroed();
        pfd.nSize = std::mem::size_of::<PIXELFORMATDESCRIPTOR>() as u16;
        pfd.nVersion = 1;
        pfd.dwFlags = PFD_DRAW_TO_WINDOW | PFD_SUPPORT_OPENGL | PFD_DOUBLEBUFFER;
        pfd.iPixelType = PFD_TYPE_RGBA;
        pfd.cColorBits = 32;
        pfd.cAlphaBits = 8;
        let fmt = ChoosePixelFormat(hdc, &pfd);
        if fmt == 0 {
            println!("ChoosePixelFormat failed");
            return;
        }
        if SetPixelFormat(hdc, fmt, &pfd) == 0 {
            println!("SetPixelFormat failed");
            return;
        }
        let legacy = wglCreateContext(hdc);
        if legacy.is_null() {
            println!("wglCreateContext failed");
            return;
        }
        if wglMakeCurrent(hdc, legacy) == 0 {
            println!("wglMakeCurrent(legacy) failed");
            return;
        }
        let create: Option<WglCreateContextAttribsArb> = load_gl(b"wglCreateContextAttribsARB\0");
        let mut hrc: HGLRC = std::ptr::null_mut();
        if let Some(create) = create {
            for (maj, min) in [(4, 5), (3, 2)] {
                let attrs = [0x2091, maj, 0x2092, min, 0x9126, 0x1, 0];
                hrc = create(hdc, std::ptr::null_mut(), attrs.as_ptr());
                if !hrc.is_null() {
                    break;
                }
            }
        }
        wglDeleteContext(legacy);
        if hrc.is_null() {
            println!("wglCreateContextAttribsARB failed");
            return;
        }
        if wglMakeCurrent(hdc, hrc) == 0 {
            println!("wglMakeCurrent(core) failed");
            return;
        }

        let get_string: GlGetString =
            load_gl(b"glGetString\0").unwrap_or(std::mem::transmute(std::ptr::null::<c_void>()));
        let version = cstr(get_string(0x1F02));
        let renderer = cstr(get_string(0x1F01));
        let vendor = cstr(get_string(0x1F00));
        println!("GL_VERSION={version}");
        println!("GL_RENDERER={renderer}");
        println!("GL_VENDOR={vendor}");

        let import: Option<unsafe extern "system" fn(u32, u64, u32, *mut c_void)> =
            load_gl(b"glImportMemoryWin32HandleEXT\0");
        let create_mem: Option<unsafe extern "system" fn(i32, *mut u32)> =
            load_gl(b"glCreateMemoryObjectsEXT\0");
        let storage: Option<unsafe extern "system" fn(u32, i32, u32, i32, i32, u32, u64)> =
            load_gl(b"glTexStorageMem2DEXT\0");
        println!(
            "GL_EXT_memory_object_win32_functions={}",
            import.is_some() && create_mem.is_some() && storage.is_some()
        );
        if let Some(geti) = load_gl::<GlGetStringi>(b"glGetStringi\0") {
            if let Some(getiv) = load_gl::<GlGetIntegerv>(b"glGetIntegerv\0") {
                let mut n = 0i32;
                getiv(0x821D, &mut n);
                let mut found = false;
                for i in 0..n {
                    let s = cstr(geti(0x1F03, i as u32));
                    if s == "GL_EXT_memory_object_win32" {
                        found = true;
                    }
                }
                println!("GL_EXT_memory_object_win32_listed={found} (extensions={n})");
            }
        }

        wglMakeCurrent(hdc, std::ptr::null_mut());
        wglDeleteContext(hrc);
        ReleaseDC(hwnd, hdc);
        DestroyWindow(hwnd);
        UnregisterClassW(windows_sys::w!("QuercusProbeWGL"), std::ptr::null_mut());
    }
}
