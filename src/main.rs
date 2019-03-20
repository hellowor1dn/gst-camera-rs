use gstreamer as gst;
use gst::prelude::*;

use std::env;
use std::error::Error as StdError;

use failure::Error;
use failure_derive::Fail;

#[derive(Debug, Fail)]
#[fail(display = "Usage: {} <device> <location>", _0)]
struct UsageError(String);

#[derive(Debug, Fail)]
#[fail(display = "Missing element {}", _0)]
struct MissingElement(&'static str);

#[derive(Debug, Fail)]
#[fail(display = "Bus watch error")]
struct WatchError;

#[derive(Debug, Fail)]
#[fail(display = "Received error from {}: {} (debug: {:?})", src, error, debug)]
struct ErrorMessage {
    src: String,
    error: String,
    debug: Option<String>,
    #[cause]
    cause: glib::Error,
}

fn make_element<'a, P: Into<Option<&'a str>>>(
    factory_name: &'static str,
    element_name: P,
) -> Result<gst::Element, Error> {
    match gst::ElementFactory::make(factory_name, element_name.into()) {
        Some(elem) => Ok(elem),
        None => Err(Error::from(MissingElement(factory_name))),
    }
}

// TODO refactor expect into error type

fn run() -> Result<(), Error> {
    // region parse args
    let args = env::args().collect::<Vec<String>>();

    if args.len() != 3 {
        return Err(Error::from(UsageError(args[0].clone())));
    }

    let device = args[1].clone();
    let location = args[2].clone();
    println!("device: {} location: {}", &device, &location);
    // endregion

    // init gstreamer
    gst::init()?;

    // init loop
    let main_loop = glib::MainLoop::new(None, false);

    // create pipeline
    let pipeline = gst::Pipeline::new("camera-recorder");

    // region create elements
    // video source
    let v4l2src: gst::Element = gst::ElementFactory::make("v4l2src", "v4l2src")
        .ok_or(MissingElement("v4l2src"))?;
    v4l2src.set_property("device", &device)?;

    // video filter
    let video_filter = make_element("capsfilter", None)?;
    let video_caps = gst::Caps::builder("image/jpeg")
        .field("width", &2592i32)
        .field("height", &1944i32)
        .build();
    video_filter.set_property("caps", &video_caps)?;

    // jpeg decoder
    let jpegdec = gst::ElementFactory::make("jpegdec", "jpegdec")
        .ok_or(MissingElement("jpegdec"))?;

    // encode queue
    let encode_queue = gst::ElementFactory::make("queue", "encode_queue")
        .ok_or(MissingElement("encode_queue"))?;

    // x264 encoder
    let x264enc = gst::ElementFactory::make("x264enc", "x264enc")
        .ok_or(MissingElement("x264enc"))?;
    x264enc.set_property("key-int-max", &10u32.to_value())?;

    // h264 filter
    let h264_filter = gst::ElementFactory::make("capsfilter", "h264_filter")
        .ok_or(MissingElement("h264_filter"))?;
    let encode_caps = gst::Caps::builder("video/x-h264")
        .field("profile", &("high"))
        .build();
    h264_filter.set_property("caps", &encode_caps)?;

    // h264 parser
    let h264parse = gst::ElementFactory::make("h264parse", "h264parse")
        .ok_or(MissingElement("h264parse"))?;

    // sink
    let splitmuxsink = gst::ElementFactory::make("splitmuxsink", "splitmuxsink")
        .ok_or(MissingElement("splitmuxsink"))?;
    splitmuxsink.set_property("location", &location)?;
    splitmuxsink.set_property("max-size-time", &10000000000u64.to_value())?;
    splitmuxsink.set_property("send-keyframe-requests", &true.to_value())?;
    // endregion

    // region set up the pipeline
    // add elements
    pipeline.add_many(&[
        &v4l2src,
        &video_filter,
        &jpegdec,
        &encode_queue,
        &x264enc,
        &h264_filter,
        &h264parse,
        &splitmuxsink,
    ])?;

    // link elements
    gst::Element::link_many(&[
        &v4l2src,
        &video_filter,
        &jpegdec,
        &encode_queue,
        &x264enc,
        &h264_filter,
        &h264parse,
        &splitmuxsink,
    ])?;
    // endregion

    // region add message handler
    let bus: gst::Bus = pipeline.get_bus()
        .expect("Pipeline doesn't have a bus (shouldn't happen)!");
    let loop_clone = main_loop.clone();
    let bus_watch_id = bus.add_watch(move |_, msg| {
        use gst::MessageView;

        println!("Got {:?} message", msg);

        match msg.view() {
            MessageView::Eos(..) => {
                println!("End of stream.");
                loop_clone.quit();
            }
            MessageView::Error(err) => {
                let error_msg = ErrorMessage {
                    src: msg
                        .get_src()
                        .map(|s| String::from(s.get_path_string()))
                        .unwrap_or_else(|| String::from("None")),
                    error: err.get_error().description().into(),
                    debug: Some(err.get_debug().unwrap().to_string()),
                    cause: err.get_error(),
                };

                eprintln!("Error: {}", error_msg);
                loop_clone.quit();
            }
            MessageView::Warning(w) => {
                let error_msg = ErrorMessage {
                    src: msg
                        .get_src()
                        .map(|s| String::from(s.get_path_string()))
                        .unwrap_or_else(|| String::from("None")),
                    error: w.get_error().description().into(),
                    debug: Some(w.get_debug().unwrap().to_string()),
                    cause: w.get_error(),
                };

                eprintln!("Warning: {}", error_msg);
            }
            MessageView::StateChanged(s) => {
                println!(
                    "State changed from {:?}: {:?} -> {:?} ({:?})",
                    s.get_src().map(|s| s.get_path_string()),
                    s.get_old(),
                    s.get_current(),
                    s.get_pending()
                );
            }
            _ => (),
        }

        glib::Continue(true)
    })
        .ok_or(WatchError)?;
    // endregion

    // start playing
    println!("Now playing");
    pipeline.set_state(gst::State::Playing)?;

    // main loop
    println!("Running...");
    main_loop.run();

    // clean up
    println!("Stopping...");
    pipeline.set_state(gst::State::Null)?;
    glib::source_remove(bus_watch_id);

    Ok(())
}

fn main() {
    match run() {
        Ok(r) => r,
        Err(e) => eprintln!("Error! {}", e)
    }
}
