#![no_std]
#![no_main]

use core::sync::atomic::{AtomicBool, Ordering};
use defmt::{info, warn};
use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_rp::bind_interrupts;
use embassy_rp::clocks::RoscRng;
use embassy_rp::gpio::{Input, Level, Output, Pull};
use embassy_rp::peripherals::USB;
use embassy_rp::usb::{Driver, InterruptHandler};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Timer};
use embassy_usb::class::hid::{HidReaderWriter, ReportId, RequestHandler, State};
use embassy_usb::control::OutResponse;
use embassy_usb::{Builder, Config, Handler};
use usbd_hid::descriptor::{MouseReport, SerializedDescriptor};
use {defmt_rtt as _, panic_probe as _};

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => InterruptHandler<USB>;
});

// Jiggle mutex i.e., on and off switch
// Enable jiggle by default
// As soon as the device is powered i.e., inserted into a USB port, it should start jiggling the cursor
type JiggleType = Mutex<CriticalSectionRawMutex, bool>;
static JIGGLE: JiggleType = Mutex::new(true);

fn generate_jiggle_vector<const N: usize>(rng_value: u32, vec: &mut heapless::Vec<i8, N>) {
    const UPPER: u8 = 32;
    const LOWER: u8 = 6;
    const STEP: i8 = 6;

    // Scale rng_value into the range [LOWER, UPPER] inclusive.
    // Use 64-bit intermediate to avoid overflow and get decent distribution.
    let range: u32 = (UPPER - LOWER) as u32;
    let scaled: u32 = if rng_value == u32::MAX {
        range
    } else {
        ((rng_value as u64 * range as u64) / (u32::MAX as u64)) as u32
    };
    let x_u8 = (LOWER as u32 + scaled) as u8;
    let mut remaining: i8 = x_u8 as i8;

    // Populate forward movement in STEP-sized chunks (last chunk may be smaller).
    while remaining > 0 && !vec.is_full() {
        let to_push: i8 = if remaining >= STEP { STEP } else { remaining };
        if vec.push(to_push).is_err() {
            break;
        }
        remaining -= to_push;
    }

    // Mirror back to origin. Iterate in reverse over current values and push negated values
    // until the vector is full.
    // Note: negating a value in the expected range (1..=16) is safe for i8.
    let clone = vec.clone();
    for &v in clone.iter().rev() {
        if vec.is_full() {
            break;
        }
        // push negated value; ignore push failure because we checked is_full above
        let _ = vec.push(-v);
    }
}

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    // Create the driver, from the HAL.
    let driver = Driver::new(p.USB, Irqs);

    // Create usb config
    // Masquerade as a Microsoft Basic Optical Mouse with a random serial number.
    let mut config = Config::new(0x045E, 0x0084);
    config.manufacturer = Some("Microsoft");
    config.product = Some("Basic Optical Mouse");
    config.serial_number = Some("SN-01-0000842");
    config.max_power = 100;
    config.max_packet_size_0 = 64;

    // Create embassy-usb DeviceBuilder using the driver and config.
    // It needs some buffers for building the descriptors.
    let mut config_descriptor = [0; 256];
    let mut bos_descriptor = [0; 256];
    // You can also add a Microsoft OS descriptor.
    let mut msos_descriptor = [0; 256];
    let mut control_buf = [0; 64];
    let mut request_handler = MyRequestHandler {};
    let mut device_handler = MyDeviceHandler::new();

    let mut state = State::new();

    let mut builder = Builder::new(
        driver,
        config,
        &mut config_descriptor,
        &mut bos_descriptor,
        &mut msos_descriptor,
        &mut control_buf,
    );

    builder.handler(&mut device_handler);

    // Create classes on the builder.
    let config = embassy_usb::class::hid::Config {
        report_descriptor: MouseReport::desc(),
        request_handler: None,
        poll_ms: 60,
        max_packet_size: 64,
    };
    let hid = HidReaderWriter::<_, 1, 8>::new(&mut builder, &mut state, config);

    // Build the builder.
    let mut usb = builder.build();

    // Run the USB device.
    let usb_fut = usb.run();

    let (reader, mut writer) = hid.split();

    let in_fut = async {
        let mut rng = RoscRng;
        let mut jiggle: bool;
        // Jiggle delay
        let duration;
        if cfg!(debug_assertions) {
            // Two seconds in debug mode
            duration = Duration::from_secs(2);
        } else {
            // a second shy of 5 mins before the next wiggle.
            // 5 mins being a typical timeout for screen savers and sleep modes.
            duration = Duration::from_secs(60 * 5 - 1);
        }
        loop {
            {
                // Check if jiggle is enabled.
                let jiggle_unlocked = JIGGLE.lock().await;
                jiggle = *jiggle_unlocked;
                // Implicit release mutex at end of inner scope
            }
            if !jiggle {
                // Jiggle is disabled, wait a bit and check again in next iteration
                _ = Timer::after_millis(1000).await;
                continue;
            }

            // To simulate more natural mouse movement, limit the maximum movement per report, and send multiple reports.
            const JIGGLE_VECTOR_SIZE: usize = 32;
            let mut jiggle_vector: heapless::Vec<i8, JIGGLE_VECTOR_SIZE> = heapless::Vec::new();
            let reverberations = 2;
            for _ in 0..reverberations {
                generate_jiggle_vector(rng.next_u32(), &mut jiggle_vector);
            }

            // See https://wiki.osdev.org/USB_Human_Interface_Devices#USB_mouse for details on mouse reports.
            // tldr: x and y are signed 8-bit integers representing relative movement.
            for x in jiggle_vector {
                // Create the mouse HID report.
                let report = MouseReport {
                    buttons: 0,
                    x: x,
                    y: 0,
                    wheel: 0,
                    pan: 0,
                };

                // Send the HID report.
                match writer.write_serialize(&report).await {
                    Ok(()) => {}
                    Err(e) => warn!("Failed to send report: {:?}", e),
                }
            }

            // Wait a before next jiggle
            _ = Timer::after(duration).await;
        }
    };

    let out_fut = async {
        reader.run(false, &mut request_handler).await;
    };

    let led_fut = async {
        let mut button = Input::new(p.PIN_23, Pull::Down);
        let mut led_r: Output<'_> = Output::new(p.PIN_18, Level::High);
        let mut led_g: Output<'_> = Output::new(p.PIN_19, Level::Low);
        let mut led_b: Output<'_> = Output::new(p.PIN_20, Level::High);
        let mut jiggle: bool;

        loop {
            // Blocking wait for BOOT button press
            button.wait_for_falling_edge().await;
            {
                // Toggle and store new jiggle state
                let mut jiggle_unlocked = JIGGLE.lock().await;
                *jiggle_unlocked = !(*jiggle_unlocked);
                jiggle = *jiggle_unlocked;
                // Implicit release mutex at end of inner scope
            }

            // Update LED color based on state
            if jiggle {
                // Jiggle enabled: green
                led_r.set_high();
                led_g.set_low();
                led_b.set_high();
            } else {
                // Jiggle disabled: off
                led_r.set_high();
                led_g.set_high();
                led_b.set_high();
            }
        }
    };

    // Run everything concurrently.
    // If we had made everything `'static` above instead, we could do this using separate tasks instead.
    join(usb_fut, join(in_fut, join(out_fut, led_fut))).await;
}

struct MyRequestHandler {}

impl RequestHandler for MyRequestHandler {
    fn get_report(&mut self, id: ReportId, _buf: &mut [u8]) -> Option<usize> {
        info!("Get report for {:?}", id);
        None
    }

    fn set_report(&mut self, id: ReportId, data: &[u8]) -> OutResponse {
        info!("Set report for {:?}: {=[u8]}", id, data);
        OutResponse::Accepted
    }

    fn set_idle_ms(&mut self, id: Option<ReportId>, dur: u32) {
        info!("Set idle rate for {:?} to {:?}", id, dur);
    }

    fn get_idle_ms(&mut self, id: Option<ReportId>) -> Option<u32> {
        info!("Get idle rate for {:?}", id);
        None
    }
}

struct MyDeviceHandler {
    configured: AtomicBool,
}

impl MyDeviceHandler {
    fn new() -> Self {
        MyDeviceHandler {
            configured: AtomicBool::new(false),
        }
    }
}

impl Handler for MyDeviceHandler {
    fn enabled(&mut self, enabled: bool) {
        self.configured.store(false, Ordering::Relaxed);
        if enabled {
            info!("Device enabled");
        } else {
            info!("Device disabled");
        }
    }

    fn reset(&mut self) {
        self.configured.store(false, Ordering::Relaxed);
        info!("Bus reset, the Vbus current limit is 100mA");
    }

    fn addressed(&mut self, addr: u8) {
        self.configured.store(false, Ordering::Relaxed);
        info!("USB address set to: {}", addr);
    }

    fn configured(&mut self, configured: bool) {
        self.configured.store(configured, Ordering::Relaxed);
        if configured {
            info!(
                "Device configured, it may now draw up to the configured current limit from Vbus."
            )
        } else {
            info!("Device is no longer configured, the Vbus current limit is 100mA.");
        }
    }
}
