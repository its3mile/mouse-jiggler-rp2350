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
use embassy_time::{Duration, Timer};
use embassy_usb::class::hid::{HidReaderWriter, ReportId, RequestHandler, State};
use embassy_usb::control::OutResponse;
use embassy_usb::{Builder, Config, Handler};
use usbd_hid::descriptor::{MouseReport, SerializedDescriptor};

use {defmt_rtt as _, panic_probe as _};

mod jiggle;

static JIGGLE_STATE: jiggle::state::State = jiggle::state::State::new();

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => InterruptHandler<USB>;
});

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
            // Should we jiggle?
            if !JIGGLE_STATE.is_enabled().await {
                // Jiggle is disabled, wait a bit and check again in next iteration
                _ = Timer::after_millis(1000).await;
                continue;
            }

            // Should we sleep?

            // To simulate more natural mouse movement, limit the maximum movement per report, and send multiple reports.
            const JIGGLE_VECTOR_SIZE: usize = 32;
            let mut jiggle_vector: heapless::Vec<i8, JIGGLE_VECTOR_SIZE> = heapless::Vec::new();
            let reverberations = 2;
            let movement = jiggle::movement::Movement::new();
            for _ in 0..reverberations {
                movement.generate_vector(rng.next_u32(), &mut jiggle_vector);
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
        let mut led_g: Output<'_> = Output::new(p.PIN_19, Level::Low);
        // Only the green LED is used, however the device powers on with both red and blue on
        // Initialise and turn off red and blue LEDs
        let _led_r: Output<'_> = Output::new(p.PIN_18, Level::High);
        let _led_b: Output<'_> = Output::new(p.PIN_20, Level::High);

        loop {
            // Blocking wait for BOOT button press
            button.wait_for_falling_edge().await;

            // Toggle and get state
            let state = JIGGLE_STATE.toggle().await;

            // Update LED color based on state
            if state {
                // Jiggle enabled: green
                led_g.set_low();
            } else {
                // Jiggle disabled: off
                led_g.set_high();
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
