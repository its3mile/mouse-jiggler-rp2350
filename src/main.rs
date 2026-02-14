#![no_std]
#![no_main]

use crate::controller::Controller;
use crate::movement::Movement;
use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration as CoreDuration;
use defmt::{info, warn};
use embassy_executor::Spawner;
use embassy_futures::join::join4;
use embassy_rp::bind_interrupts;
use embassy_rp::clocks::RoscRng;
use embassy_rp::gpio::{Input, Pull};
use embassy_rp::peripherals::{PIO0, PIO1, PIO2, USB};
use embassy_rp::pio::Instance;
use embassy_rp::pio::{InterruptHandler as PioInterruptHandler, Pio};
use embassy_rp::pio_programs::pwm::{PioPwm, PioPwmProgram};
use embassy_rp::usb::{Driver, InterruptHandler as UsbInterruptHandler};
use embassy_time::{Duration, Instant, Timer, with_deadline};
use embassy_usb::class::hid::{HidReaderWriter, ReportId, RequestHandler, State};
use embassy_usb::control::OutResponse;
use embassy_usb::{Builder, Config, Handler};
use usbd_hid::descriptor::{MouseReport, SerializedDescriptor};

use {defmt_rtt as _, panic_probe as _};

mod controller;
mod movement;

// 1 second cycle
const CYCLE_DURATION: Duration = Duration::from_secs(1);

// Jiggle controller
// timeout is 2 seconds in debug mode and 179 seconds otherwise
static CONTROLLER: Controller = controller::Controller::new(
    true,
    if cfg!(debug_assertions) {
        Duration::from_secs(2)
    } else {
        Duration::from_secs(60 * 3 - 1)
    },
    CYCLE_DURATION,
);

// PWM period, which is the length of time for each pio wave until reset.
// This is set to 255 to mimic RGB values, this simplifies the scaling for setting LED intensity
const REFRESH_INTERVAL: u64 = u8::MAX as u64;

bind_interrupts!(struct UsbIrqs {
    USBCTRL_IRQ => UsbInterruptHandler<USB>;
});

bind_interrupts!(struct PioIrqs {
    PIO0_IRQ_0 => PioInterruptHandler<PIO0>;
    PIO1_IRQ_0 => PioInterruptHandler<PIO1>;
    PIO2_IRQ_0 => PioInterruptHandler<PIO2>;
});

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    // Create the driver, from the HAL.
    let driver = Driver::new(p.USB, UsbIrqs);

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
        loop {
            // Feed controller
            if CONTROLLER.feed().await {
                // To simulate more natural mouse movement, limit the maximum movement per report, and send multiple reports.
                let reverberations = 2;
                const JIGGLE_VECTOR_SIZE: usize = 64;
                let mut jiggle_vector_v: heapless::Vec<i8, JIGGLE_VECTOR_SIZE> =
                    heapless::Vec::new();
                let mut jiggle_vector_h: heapless::Vec<i8, JIGGLE_VECTOR_SIZE> =
                    heapless::Vec::new();
                let movement = Movement::new();
                for _ in 0..reverberations {
                    movement.generate_vector(rng.next_u32(), &mut jiggle_vector_v);
                    movement.generate_vector(rng.next_u32(), &mut jiggle_vector_h);
                }

                // See https://wiki.osdev.org/USB_Human_Interface_Devices#USB_mouse for details on mouse reports.
                // tldr: x and y are signed 8-bit integers representing relative movement.
                for (x, y) in jiggle_vector_h.iter().zip(jiggle_vector_v.iter()) {
                    // Create the mouse HID report.
                    let report = MouseReport {
                        buttons: 0,
                        x: *x,
                        y: *y,
                        wheel: 0,
                        pan: 0,
                    };

                    // Send the HID report.
                    match writer.write_serialize(&report).await {
                        Ok(()) => {}
                        Err(e) => warn!("Failed to send report: {:?}", e),
                    }
                }
            }

            // Wait for next cycle
            Timer::after(CYCLE_DURATION).await;
        }
    };

    let out_fut = async {
        reader.run(false, &mut request_handler).await;
    };

    let led_fut = async {
        // Initialise BOOT button
        let mut button = Input::new(p.PIN_23, Pull::Down);

        // Initialise R, G, and B LEDs with PWM control
        // RGB LEDs are connected to GP18-GP20 and active low on the Pimoroni

        // Red
        let pio_led1 = Pio::new(p.PIO0, PioIrqs);
        let Pio {
            common: mut common_r,
            sm0: sm0_r,
            ..
        } = pio_led1;
        let prg_r: PioPwmProgram<'_, PIO0> = PioPwmProgram::new(&mut common_r);
        let mut pwm_pio_r: PioPwm<'_, PIO0, 0> =
            PioPwm::new(&mut common_r, sm0_r, p.PIN_18, &prg_r);
        pwm_pio_r.set_period(CoreDuration::from_micros(REFRESH_INTERVAL));
        pwm_pio_r.start();

        // Green
        let pio_led2: Pio<'_, PIO1> = Pio::new(p.PIO1, PioIrqs);
        let Pio {
            common: mut common_g,
            sm0: sm0_g,
            ..
        } = pio_led2;
        let prg_g = PioPwmProgram::new(&mut common_g);
        let mut pwm_pio_g = PioPwm::new(&mut common_g, sm0_g, p.PIN_19, &prg_g);
        pwm_pio_g.set_period(CoreDuration::from_micros(REFRESH_INTERVAL));
        pwm_pio_g.start();

        // Blue
        let pio_led3 = Pio::new(p.PIO2, PioIrqs);
        let Pio {
            common: mut common_b,
            sm0: sm0_b,
            ..
        } = pio_led3;
        let prg_b = PioPwmProgram::new(&mut common_b);
        let mut pwm_pio_b = PioPwm::new(&mut common_b, sm0_b, p.PIN_20, &prg_b);
        pwm_pio_b.set_period(CoreDuration::from_micros(REFRESH_INTERVAL));
        pwm_pio_b.start();

        // Set initial LED colour - green
        set_led(&mut pwm_pio_r, &mut pwm_pio_g, &mut pwm_pio_b, 0, 255, 0);

        loop {
            // Blocking wait for BOOT button press
            button.wait_for_falling_edge().await;

            // Get start instant
            let start = Instant::now();

            // Check for a second falling edge within 300ms (a double press)
            let is_double_press = match with_deadline(
                start + Duration::from_millis(300),
                button.wait_for_falling_edge(),
            )
            .await
            {
                Ok(_) => true,
                Err(_) => false,
            };

            // Handle button press
            if is_double_press {
                // Double press - Do something ...
            } else {
                // Single press - on and off button
                // Toggle controller state and update LED color based on it
                if CONTROLLER.toggle().await {
                    set_led(&mut pwm_pio_r, &mut pwm_pio_g, &mut pwm_pio_b, 0, 255, 0);
                } else {
                    set_led(&mut pwm_pio_r, &mut pwm_pio_g, &mut pwm_pio_b, 0, 0, 0);
                }
            }
        }
    };

    // Run everything concurrently.
    join4(usb_fut, in_fut, out_fut, led_fut).await;
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

/// function to simplify setting RGB LEDs
fn set_led<'d, PioR, PioG, PioB, const SM_R: usize, const SM_G: usize, const SM_B: usize>(
    pwm_pio_r: &mut PioPwm<'d, PioR, SM_R>, // Red PWM channel
    pwm_pio_g: &mut PioPwm<'d, PioG, SM_G>, // Green PWM channel
    pwm_pio_b: &mut PioPwm<'d, PioB, SM_B>, // Blue PWM channel
    red: u8,                                // Red brightness (0-255)
    green: u8,                              // Green brightness (0-255)
    blue: u8,                               // Blue brightness (0-255)
) where
    PioR: Instance,
    PioG: Instance,
    PioB: Instance,
{
    pwm_pio_r.write(CoreDuration::from_micros((u8::MAX - red) as u64));
    pwm_pio_g.write(CoreDuration::from_micros((u8::MAX - green) as u64));
    pwm_pio_b.write(CoreDuration::from_micros((u8::MAX - blue) as u64));
}
