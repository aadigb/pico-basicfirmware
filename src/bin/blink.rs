//! Pico 2 W: USB serial "hello" once a second + onboard LED blink.

#![no_std]
#![no_main]

use cyw43::aligned_bytes;
use cyw43_pio::{PioSpi, RM2_CLOCK_DIVIDER};
use embassy_executor::Spawner;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::peripherals::{DMA_CH0, PIO0};
use embassy_rp::pio::{InterruptHandler, Pio};
use embassy_rp::{bind_interrupts, dma};
use embassy_time::{Instant, Timer};
use pico_firmware::{report_previous_panic, sprintln, start_usb, terminal_open};
use portable_atomic::{AtomicU8, Ordering};
use static_cell::StaticCell;

#[unsafe(link_section = ".bi_entries")]
#[used]
pub static PICOTOOL_ENTRIES: [embassy_rp::binary_info::EntryAddr; 4] = [
    embassy_rp::binary_info::rp_program_name!(c"pico2w-blink"),
    embassy_rp::binary_info::rp_program_description!(c"USB serial hello + CYW43 LED blink"),
    embassy_rp::binary_info::rp_cargo_version!(),
    embassy_rp::binary_info::rp_program_build_attribute!(),
];

bind_interrupts!(struct Irqs {
    PIO0_IRQ_0 => InterruptHandler<PIO0>;
    DMA_IRQ_0 => dma::InterruptHandler<DMA_CH0>;
});

// 0 = starting, 1 = LED blinking
static LED_STATE: AtomicU8 = AtomicU8::new(0);

#[embassy_executor::task]
async fn cyw43_task(runner: cyw43::Runner<'static, cyw43::SpiBus<Output<'static>, PioSpi<'static, PIO0, 0>>>) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn hello_task() -> ! {
    let mut n: u32 = 0;
    loop {
        if terminal_open() {
            if n == 0 || n % 10 == 0 {
                report_previous_panic().await;
            }
            let led = match LED_STATE.load(Ordering::Relaxed) {
                1 => "blinking",
                _ => "starting Wi-Fi chip (LED not yet on)",
            };
            sprintln!(
                "hello #{} from Pico 2 W (RP2350, clk_sys={} MHz, uptime {} ms, LED: {})",
                n,
                embassy_rp::clocks::clk_sys_freq() / 1_000_000,
                Instant::now().as_millis(),
                led
            );
            n += 1;
        }
        Timer::after_millis(1000).await;
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());

    // USB first: it's how we see what's happening.
    start_usb(p.USB, "Pico2W blink + hello");
    spawner.spawn(hello_task().unwrap());

    // The LED is on the Wi-Fi chip, so load its firmware first.
    let fw = aligned_bytes!("../../cyw43-firmware/43439A0.bin");
    let clm = aligned_bytes!("../../cyw43-firmware/43439A0_clm.bin");
    let nvram = aligned_bytes!("../../cyw43-firmware/nvram_rp2040.bin");

    let pwr = Output::new(p.PIN_23, Level::Low);
    let cs = Output::new(p.PIN_25, Level::High);
    let mut pio = Pio::new(p.PIO0, Irqs);
    let spi = PioSpi::new(
        &mut pio.common,
        pio.sm0,
        RM2_CLOCK_DIVIDER,
        pio.irq0,
        cs,
        p.PIN_24,
        p.PIN_29,
        dma::Channel::new(p.DMA_CH0, Irqs),
    );

    static STATE: StaticCell<cyw43::State> = StaticCell::new();
    let state = STATE.init(cyw43::State::new());
    let (_net_device, mut control, runner) = cyw43::new(state, pwr, spi, fw, nvram).await;
    spawner.spawn(cyw43_task(runner).unwrap());
    control.init(clm).await;
    control.set_power_management(cyw43::PowerManagementMode::PowerSave).await;
    LED_STATE.store(1, Ordering::Relaxed);

    loop {
        control.gpio_set(0, true).await;
        Timer::after_millis(250).await;
        control.gpio_set(0, false).await;
        Timer::after_millis(250).await;
    }
}