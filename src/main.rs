use clap::Parser;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample};
use std::process::Command;
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use chrono::prelude::*;
use std::str;

#[derive(Parser, Debug)]
#[command(version, about = "CPAL record_wav example", long_about = None)]
struct Opt {
    /// The audio device to use
    #[arg(short, long, default_value_t = String::from("default"))]
    device: String,

    /// Use the JACK host
    #[cfg(all(
        any(
            target_os = "linux",
            target_os = "dragonfly",
            target_os = "freebsd",
            target_os = "netbsd"
        ),
        feature = "jack"
    ))]
    #[arg(short, long)]
    #[allow(dead_code)]
    jack: bool,
}

const RECORD_SECONDS: u64 = 60;
const BASE_FOLDER: &str = "/home/orangepi/workspace/audio_recordings";
const MAX_FOLDER_SIZE: u64 = 2 * 1024 * 1024 * 1024;

fn get_folder_size(folder: &Path) -> std::io::Result<u64> {
    let output = Command::new("du")
        .arg("-sb")
        .arg(folder)
        .output()
        .expect("Failed to execute du command");

    if output.status.success() {
        let output_str = str::from_utf8(&output.stdout)
            .expect("Failed to convert output to UTF-8");
        let size_str = output_str.split_whitespace().next()
            .expect("No output from du command");
        let size: u64 = size_str.parse()
            .expect("Failed to parse size from du output");
        println!("Folder size: {}", size);
        Ok(size)
    } else {
        let error_message = str::from_utf8(&output.stderr)
            .expect("Failed to convert error output to UTF-8");
        panic!("Error calculating folder size: {}", error_message);
    }
}

fn get_oldest_directory(folder: &Path) -> std::io::Result<Option<PathBuf>> {
    let mut directories: Vec<_> = fs::read_dir(folder)?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let metadata = entry.metadata().ok()?;
            if metadata.is_dir() {
                Some((entry.path(), metadata.modified().ok()?))
            } else {
                None
            }
        })
        .collect();

    directories.sort_by_key(|&(_, modified)| modified);
    Ok(directories.first().map(|(path, _)| path.clone()))
}

fn get_oldest_file_in_directory(directory: &Path) -> std::io::Result<Option<PathBuf>> {
    let mut files: Vec<_> = fs::read_dir(directory)?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let metadata = entry.metadata().ok()?;
            if metadata.is_file() {
                Some((entry.path(), metadata.modified().ok()?))
            } else {
                None
            }
        })
        .collect();

    files.sort_by_key(|&(_, modified)| modified);
    Ok(files.first().map(|(path, _)| path.clone()))
}


fn delete_oldest_file(folder: &Path) -> std::io::Result<()> {
    let mut retry = true;
    while retry {
        retry = false;

        if let Some(oldest_directory) = get_oldest_directory(folder)? {
            if let Some(oldest_file) = get_oldest_file_in_directory(&oldest_directory)? {
                fs::remove_file(&oldest_file)?;
                println!("Deleted oldest file: {:?}", oldest_file);
            } else {
                // No files found in the oldest directory, so delete the directory
                fs::remove_dir(&oldest_directory)?;
                println!("Deleted empty directory: {:?}", oldest_directory);

                // Retry deleting the oldest file in the folder containing audio files
                retry = true;
            }
        } else {
            println!("No directories found.");
        }
    }
    Ok(())
}

fn ensure_folder_size(folder: &Path, max_size: u64) -> std::io::Result<()> {
    while get_folder_size(folder)? > max_size {
	println!("trying to delete old files");
        delete_oldest_file(folder)?;
    }
    Ok(())
}

fn main() -> Result<(), anyhow::Error> {
    let opt = Opt::parse();

    #[cfg(all(
        any(
            target_os = "linux",
            target_os = "dragonfly",
            target_os = "freebsd",
            target_os = "netbsd"
        ),
        feature = "jack"
    ))]
    let host = if opt.jack {
        cpal::host_from_id(cpal::available_hosts()
            .into_iter()
            .find(|id| *id == cpal::HostId::Jack)
            .expect(
                "make sure --features jack is specified. only works on OSes where jack is available",
            )).expect("jack host unavailable")
    } else {
        cpal::default_host()
    };

    #[cfg(any(
        not(any(
            target_os = "linux",
            target_os = "dragonfly",
            target_os = "freebsd",
            target_os = "netbsd"
        )),
        not(feature = "jack")
    ))]
    let host = cpal::default_host();

    let device = if opt.device == "default" {
        host.default_input_device()
    } else {
        host.input_devices()?
            .find(|x| x.name().map(|y| y == opt.device).unwrap_or(false))
    }
    .expect("failed to find input device");

    println!("Input device: {}", device.name()?);

    let config = device
        .default_input_config()
        .expect("Failed to get default input config");
    println!("Default input config: {:?}", config);

    loop {
        let date_folder = Local::now().format("%Y-%m-%d").to_string();
        let save_folder = Path::new(BASE_FOLDER).join(&date_folder);
        if !save_folder.exists() {
            fs::create_dir_all(&save_folder)?;
        }

        let timestamp = Local::now().format("%H-%M").to_string();
        let filename = save_folder.join(&timestamp).with_extension("wav");

        ensure_folder_size(Path::new(BASE_FOLDER), MAX_FOLDER_SIZE)?;

        let spec = wav_spec_from_config(&config);
        let writer = hound::WavWriter::create(&filename, spec)?;
        let writer = Arc::new(Mutex::new(Some(writer)));

        println!("Begin recording...");

        let writer_2 = writer.clone();

        let err_fn = move |err| {
            eprintln!("an error occurred on stream: {}", err);
        };

        let stream_config: cpal::StreamConfig = config.clone().into();

        let stream = match config.sample_format() {
            cpal::SampleFormat::I8 => device.build_input_stream(
                &stream_config,
                move |data, _: &_| write_input_data::<i8, i8>(data, &writer_2),
                err_fn,
                None,
            )?,
            cpal::SampleFormat::I16 => device.build_input_stream(
                &stream_config,
                move |data, _: &_| write_input_data::<i16, i16>(data, &writer_2),
                err_fn,
                None,
            )?,
            cpal::SampleFormat::I32 => device.build_input_stream(
                &stream_config,
                move |data, _: &_| write_input_data::<i32, i32>(data, &writer_2),
                err_fn,
                None,
            )?,
            cpal::SampleFormat::F32 => device.build_input_stream(
                &stream_config,
                move |data, _: &_| write_input_data::<f32, f32>(data, &writer_2),
                err_fn,
                None,
            )?,
            sample_format => {
                return Err(anyhow::Error::msg(format!(
                    "Unsupported sample format '{sample_format}'"
                )))
            }
        };

        stream.play()?;

        std::thread::sleep(Duration::from_secs(RECORD_SECONDS));
        drop(stream);
        writer.lock().unwrap().take().unwrap().finalize()?;
        println!("Recording {:?} complete!", filename);

        std::thread::sleep(Duration::from_secs(1));
    }
}

fn sample_format(format: cpal::SampleFormat) -> hound::SampleFormat {
    if format.is_float() {
        hound::SampleFormat::Float
    } else {
        hound::SampleFormat::Int
    }
}

fn wav_spec_from_config(config: &cpal::SupportedStreamConfig) -> hound::WavSpec {
    hound::WavSpec {
        channels: config.channels() as _,
        sample_rate: config.sample_rate().0 as _,
        bits_per_sample: (config.sample_format().sample_size() * 8) as _,
        sample_format: sample_format(config.sample_format()),
    }
}

type WavWriterHandle = Arc<Mutex<Option<hound::WavWriter<BufWriter<File>>>>>;

fn write_input_data<T, U>(input: &[T], writer: &WavWriterHandle)
where
    T: Sample,
    U: Sample + hound::Sample + FromSample<T>,
{
    if let Ok(mut guard) = writer.try_lock() {
        if let Some(writer) = guard.as_mut() {
            for &sample in input.iter() {
                let sample: U = U::from_sample(sample);
                writer.write_sample(sample).ok();
            }
        }
    }
}
