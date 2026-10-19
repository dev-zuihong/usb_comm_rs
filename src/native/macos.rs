//! 在专用线程的 CFRunLoop 中监听 IOKit HID/串口服务。

use super::*;
use core_foundation::runloop::*;
use io_kit_sys::types::io_iterator_t;
use io_kit_sys::*;
use std::{ffi::c_void, ptr};

unsafe extern "C" fn changed(context: *mut c_void, iterator: io_iterator_t) {
    // 必须耗尽首次及后续通知的迭代器，IOKit 才会继续发送通知。
    loop {
        let object = unsafe { IOIteratorNext(iterator) };
        if object == 0 {
            break;
        }
        unsafe {
            IOObjectRelease(object);
        }
    }
    // SAFETY: context 保留在监听线程中，直到全部注册和 run loop source 被移除。
    unsafe { &*context.cast::<Signal>() }.notify();
}

pub(super) fn run(
    stop: &AtomicBool,
    signal: &Signal,
    ready: mpsc::SyncSender<Result<()>>,
) -> Result<()> {
    // IOKit 和 CoreFoundation 对象均只在本线程内访问和释放。
    unsafe {
        let port = IONotificationPortCreate(kIOMasterPortDefault);
        if port.is_null() {
            let error = Error::Backend {
                backend: "IOKit",
                message: "notification port creation failed".into(),
            };
            let _ = ready.send(Err(error.clone()));
            return Err(error);
        }
        let source = IONotificationPortGetRunLoopSource(port);
        if source.is_null() {
            IONotificationPortDestroy(port);
            let error = Error::Backend {
                backend: "IOKit",
                message: "run loop source unavailable".into(),
            };
            let _ = ready.send(Err(error.clone()));
            return Err(error);
        }
        let run_loop = CFRunLoopGetCurrent();
        CFRunLoopAddSource(run_loop, source, kCFRunLoopDefaultMode);
        let mut context = signal.clone();
        let context_ptr = ptr::from_mut(&mut context).cast();
        let mut registrations = Vec::new();
        let mut result = Ok(());
        for class in [c"IOHIDDevice", c"IOSerialBSDClient"] {
            for notification in [c"IOServiceFirstMatch", c"IOServiceTerminate"] {
                let matching = IOServiceMatching(class.as_ptr());
                let mut iterator = 0;
                let status = IOServiceAddMatchingNotification(
                    port,
                    notification.as_ptr().cast_mut(),
                    matching,
                    changed,
                    context_ptr,
                    &mut iterator,
                );
                if status != 0 {
                    result = Err(Error::Backend {
                        backend: "IOKit",
                        message: format!("matching notification failed: {status:#x}"),
                    });
                    break;
                }
                registrations.push(iterator);
                changed(context_ptr, iterator);
            }
            if result.is_err() {
                break;
            }
        }
        let _ = ready.send(result.clone());
        if result.is_ok() {
            while !stop.load(Ordering::Acquire) {
                CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.1, 1);
            }
        }
        CFRunLoopRemoveSource(run_loop, source, kCFRunLoopDefaultMode);
        for iterator in registrations {
            IOObjectRelease(iterator);
        }
        IONotificationPortDestroy(port);
        result
    }
}
