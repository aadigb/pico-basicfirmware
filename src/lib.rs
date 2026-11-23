//! USB-serial console + crash reporter for the Pico 2 W.

#![no_std]

use core::fmt::Write as _;
use core::sync::atomic::{AtomicBool, Ordering};

use embassy_executor::InterruptExecutor;
use embassy_rp::interrupt::{InterruptExt, Priority};
use embassy_rp::peripherals::USB;
use embassy_rp::usb::{Driver, InterruptHandler};
use embassy_rp::{Peri, bind_interrupts, interrupt};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::pipe::Pipe;
use embassy_time::{Duration, Timer, with_timeout};
use embassy_usb::class::cdc_acm::{CdcAcmClass, ControlChanged, Sender, State};
use embassy_futures::join::join3;

bind_interrupts!(struct UsbIrqs {
    USBCTRL_IRQ => InterruptHandler<USB>;
});

static EXECUTOR_USB: InterruptExecutor = InterruptExecutor::new();

#[interrupt]
unsafe fn SWI_IRQ_0() {
    unsafe { EXECUTOR_USB.on_interrupt() }
}

static OUT: Pipe<CriticalSectionRawMutex, 8192> = Pipe::new();
static DTR: AtomicBool = AtomicBool::new(false);

/// True while a serial terminal has the port open (DTR asserted).
pub fn terminal_open() -> bool {
    DTR.load(Ordering::Relaxed)
}

/// Wait until a terminal opens the port, or `timeout_s` seconds pass.
pub async fn wait_for_terminal(timeout_s: u64) -> bool {
    let mut waited_ms = 0u64;
    while !terminal_open() {
        if waited_ms >= timeout_s * 1000 {
            return false;
        }
        Timer::after_millis(50).await;
        waited_ms += 50;
    }
    Timer::after_millis(300).await;
    true
}
/// Start the USB console. Call once, right after `embassy_rp::init`.
pub fn start_usb(usb: Peri<'static, USB>, product: &'static str) {
    interrupt::SWI_IRQ_0.set_priority(Priority::P2);
    let spawner = EXECUTOR_USB.start(interrupt::SWI_IRQ_0);
    spawner.spawn(usb_main(usb, product).unwrap());
}

#[embassy_executor::task]
async fn usb_main(usb: Peri<'static, USB>, product: &'static str) -> ! {
    let driver = Driver::new(usb, UsbIrqs);
    let mut config = embassy_usb::Config::new(0xc0de, 0xcafe); // embassy test VID/PID
    config.manufacturer = Some("UROP pq-wallet");
    config.product = Some(product);
    config.serial_number = Some("PICO2W-PQ-TEST");
    config.max_power = 100;
    config.max_packet_size_0 = 64;

    let mut config_desc = [0u8; 256];
    let mut bos_desc = [0u8; 256];
    let mut control_buf = [0u8; 64];
    let mut state = State::new();

    let mut builder = embassy_usb::Builder::new(
        driver,
        config,
        &mut config_desc,
        &mut bos_desc,
        &mut [],
        &mut control_buf,
    );
    let class = CdcAcmClass::new(&mut builder, &mut state, 64);
    let (sender, _receiver, control) = class.split_with_control();
    let mut usb = builder.build();

    join3(usb.run(), dtr_loop(control), writer_loop(sender)).await;
    unreachable!()
}

async fn dtr_loop(control: ControlChanged<'_>) -> ! {
    loop {
        DTR.store(control.dtr(), Ordering::Relaxed);
        control.control_changed().await;
    }
}

async fn writer_loop<'d>(mut tx: Sender<'d, Driver<'d, USB>>) -> ! {
    let mut buf = [0u8; 63];
    loop {
        let n = OUT.read(&mut buf).await;
        if !terminal_open() {
            continue;
        }
        if with_timeout(Duration::from_millis(200), tx.write_packet(&buf[..n])).await.is_err() {
            // Host not reading; drop rather than stall.
        }
    }
}

/// Queue raw bytes for the console.
pub async fn write_bytes(b: &[u8]) {
    if terminal_open() {
        OUT.write_all(b).await;
    }
}

/// Format and queue a line.
pub async fn print_fmt(args: core::fmt::Arguments<'_>, newline: bool) {
    let mut s: heapless::String<258> = heapless::String::new();
    let _ = s.write_fmt(args);
    if newline {
        let _ = s.push_str("\r\n");
    }
    write_bytes(s.as_bytes()).await;
}

#[macro_export]
macro_rules! sprintln {
    () => { $crate::print_fmt(format_args!(""), true).await };
    ($($arg:tt)*) => { $crate::print_fmt(format_args!($($arg)*), true).await };
}

#[macro_export]
macro_rules! sprint {
    ($($arg:tt)*) => { $crate::print_fmt(format_args!($($arg)*), false).await };
}
// Crash reporting: panic -> save message in RAM that survives reset -> reset -> print on next boot.

const PANIC_MAGIC: u32 = 0x5041_4E43; // "PANC"

#[repr(C)]
struct PanicRecord {
    magic: u32,
    len: u32,
    msg: [u8; 480],
}

#[unsafe(link_section = ".uninit.PANIC")]
static mut PANIC_REC: core::mem::MaybeUninit<PanicRecord> = core::mem::MaybeUninit::uninit();

struct RecWriter<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl core::fmt::Write for RecWriter<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for &b in s.as_bytes() {
            if self.len < self.buf.len() {
                self.buf[self.len] = b;
                self.len += 1;
            }
        }
        Ok(())
    }
}

/// If the previous run panicked, return its message (and clear it).
pub fn take_panic_message(out: &mut [u8; 480]) -> Option<usize> {
    unsafe {
        let rec = &mut *(&raw mut PANIC_REC as *mut PanicRecord);
        if rec.magic == PANIC_MAGIC && (rec.len as usize) <= rec.msg.len() {
            let n = rec.len as usize;
            out[..n].copy_from_slice(&rec.msg[..n]);
            rec.magic = 0;
            Some(n)
        } else {
            rec.magic = 0;
            None
        }
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    cortex_m::interrupt::disable();
    unsafe {
        let rec = &mut *(&raw mut PANIC_REC as *mut PanicRecord);
        let mut w = RecWriter { buf: &mut rec.msg, len: 0 };
        let _ = write!(w, "{}", info);
        rec.len = w.len as u32;
        rec.magic = PANIC_MAGIC;
    }
    cortex_m::asm::dsb();
    cortex_m::peripheral::SCB::sys_reset();
}

/// Print a saved panic message (if any). Returns true if there was one.
pub async fn report_previous_panic() -> bool {
    let mut buf = [0u8; 480];
    if let Some(n) = take_panic_message(&mut buf) {
        let msg = core::str::from_utf8(&buf[..n]).unwrap_or("<non-utf8 panic message>");
        sprintln!("!!! PREVIOUS RUN PANICKED (chip was reset): {}", msg);
        true
    } else {
        false
    }
}