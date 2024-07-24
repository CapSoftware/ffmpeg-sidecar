use std::{
    fs::{ create_dir_all, read_dir, remove_dir_all, remove_file, rename },
    io::Read,
    path::{ Path, PathBuf },
    process::{ Command, ExitStatus, Stdio },
};

use anyhow::Context;

use crate::{ command::ffmpeg_is_installed, paths::sidecar_dir };

pub const UNPACK_DIRNAME: &str = "ffmpeg_release_temp";

/// URL of a manifest file containing the latest published build of FFmpeg. The
/// correct URL for the target platform is baked in at compile time.
pub fn ffmpeg_manifest_url() -> anyhow::Result<&'static str> {
    if cfg!(not(target_arch = "x86_64")) {
        anyhow::bail!("Downloads must be manually provided for non-x86_64 architectures");
    }

    if cfg!(target_os = "windows") {
        Ok("https://www.gyan.dev/ffmpeg/builds/release-version")
    } else if cfg!(target_os = "macos") {
        Ok("https://evermeet.cx/ffmpeg/info/ffmpeg/release")
    } else if cfg!(target_os = "linux") {
        Ok("https://johnvansickle.com/ffmpeg/release-readme.txt")
    } else {
        anyhow::bail!("Unsupported platform")
    }
}

/// URL for the latest published FFmpeg release. The correct URL for the target
/// platform is baked in at compile time.
pub fn ffmpeg_download_url() -> anyhow::Result<&'static str> {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Ok("https://cap-ffmpeg.s3.amazonaws.com/ffmpeg-7.0.1-essentials_build.zip")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Ok("https://cap-ffmpeg.s3.amazonaws.com/ffmpeg-release-amd64-static.tar.xz")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Ok("https://cap-ffmpeg.s3.amazonaws.com/ffmpeg-7.0.1.zip")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Ok("https://cap-ffmpeg.s3.amazonaws.com/ffmpegarm.zip") // Mac M1
    } else {
        anyhow::bail!(
            "Unsupported platform; you can provide your own URL instead and call download_ffmpeg_package directly."
        )
    }
}

/// Check if FFmpeg is installed, and if it's not, download and unpack it.
/// Automatically selects the correct binaries for Windows, Linux, and MacOS.
/// The binaries will be placed in the same directory as the Rust executable.
///
/// If FFmpeg is already installed, the method exits early without downloading
/// anything.
pub fn auto_download() -> anyhow::Result<()> {
    if ffmpeg_is_installed() {
        return Ok(());
    }

    let download_url = ffmpeg_download_url()?;
    let destination = sidecar_dir()?;
    let archive_path = download_ffmpeg_package(download_url, &destination)?;
    unpack_ffmpeg(&archive_path, &destination)?;

    if !ffmpeg_is_installed() {
        anyhow::bail!("FFmpeg failed to install, please install manually.");
    }

    Ok(())
}

/// Parse the the MacOS version number from a JSON string manifest file.
///
/// Example input: https://evermeet.cx/ffmpeg/info/ffmpeg/release
///
/// ```rust
/// use ffmpeg_sidecar::download::parse_macos_version;
/// let json_string = "{\"name\":\"ffmpeg\",\"type\":\"release\",\"version\":\"6.0\",...}";
/// let parsed = parse_macos_version(&json_string).unwrap();
/// assert!(parsed == "6.0");
/// ```
pub fn parse_macos_version(version: &str) -> Option<String> {
    version
        .split("\"version\":")
        .nth(1)?
        .trim()
        .split('\"')
        .nth(1)
        .map(|s| s.to_string())
}

/// Parse the the Linux version number from a long manifest text file.
///
/// Example input: https://johnvansickle.com/ffmpeg/release-readme.txt
///
/// ```rust
/// use ffmpeg_sidecar::download::parse_linux_version;
/// let json_string = "build: ffmpeg-5.1.1-amd64-static.tar.xz\nversion: 5.1.1\n\ngcc: 8.3.0";
/// let parsed = parse_linux_version(&json_string).unwrap();
/// assert!(parsed == "5.1.1");
/// ```
pub fn parse_linux_version(version: &str) -> Option<String> {
    version
        .split("version:")
        .nth(1)?
        .split_whitespace()
        .next()
        .map(|s| s.to_string())
}

/// Invoke cURL on the command line to download a file, returning it as a string.
pub fn curl(url: &str) -> anyhow::Result<String> {
    let mut child = Command::new("curl")
        .args(["-L", url])
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()?;

    let stdout = child.stdout.take().context("Failed to get stdout")?;

    let mut string = String::new();
    std::io::BufReader::new(stdout).read_to_string(&mut string)?;
    Ok(string)
}

/// Invoke cURL on the command line to download a file, writing to a file.
pub fn curl_to_file(url: &str, destination: &str) -> anyhow::Result<ExitStatus> {
    Command::new("curl").args(["-L", url]).args(["-o", destination]).status().map_err(Into::into)
}

/// Makes an HTTP request to obtain the latest version available online,
/// automatically choosing the correct URL for the current platform.
pub fn check_latest_version() -> anyhow::Result<String> {
    let string = curl(ffmpeg_manifest_url()?)?;

    if cfg!(target_os = "windows") {
        Ok(string)
    } else if cfg!(target_os = "macos") {
        parse_macos_version(&string).context("failed to parse version number (macos variant)")
    } else if cfg!(target_os = "linux") {
        parse_linux_version(&string).context("failed to parse version number (linux variant)")
    } else {
        Err(anyhow::Error::msg("Unsupported platform"))
    }
}

/// Invoke `curl` to download an archive (ZIP on windows, TAR on linux and mac)
/// from the latest published release online.
pub fn download_ffmpeg_package(url: &str, download_dir: &Path) -> anyhow::Result<PathBuf> {
    let filename = Path::new(url).file_name().context("Failed to get filename")?;

    let archive_path = download_dir.join(filename);

    let archive_filename = archive_path.to_str().context("invalid download path")?;

    let exit_status = curl_to_file(url, archive_filename)?;

    if !exit_status.success() {
        anyhow::bail!("Failed to download ffmpeg");
    }

    Ok(archive_path)
}

/// After downloading, unpacks the archive to a folder, moves the binaries to
/// their final location, and deletes the archive and temporary folder.
// After downloading, unpacks the archive to a folder, moves the binaries to
// their final location, and deletes the archive and temporary folder.
pub fn unpack_ffmpeg(from_archive: &PathBuf, binary_folder: &Path) -> anyhow::Result<()> {
    let temp_dirname = UNPACK_DIRNAME;
    let temp_folder = binary_folder.join(temp_dirname);

    println!("Unpacking ffmpeg from {:?} to {:?}", from_archive, temp_folder);
    create_dir_all(&temp_folder)?;

    println!("Extracting archive");

    let extension = from_archive.extension().and_then(std::ffi::OsStr::to_str).unwrap_or("");
    println!("Extension: {:?}", extension);

    // Determine the command based on the file extension
    let mut unpack_command = match extension {
        "zip" => Command::new("unzip"),
        "tar" | "xz" | "gz" => Command::new("tar"),
        _ => anyhow::bail!("Unsupported archive format"),
    };

    // Set arguments based on the command
    let unpack_args = match extension {
        "zip" => vec!["-o", from_archive.to_str().unwrap(), "-d", temp_folder.to_str().unwrap()],
        "tar" | "xz" | "gz" =>
            vec!["-xf", from_archive.to_str().unwrap(), "-C", temp_folder.to_str().unwrap()],
        _ => vec![],
    };

    println!("Unpacking command: {:?}", unpack_command);
    println!("Unpacking args: {:?}", unpack_args);

    // Log what files are inside the temp folder
    let files = read_dir(&temp_folder)?
        .filter_map(|entry| entry.ok()) // Filter out any errors and unwrap the Result
        .map(|entry| entry.path()) // Get the path of each entry
        .collect::<Vec<_>>(); // Collect paths into a Vec

    println!("Files: {:?}", files);

    println!("Running command: {:?} {}", unpack_command, unpack_args.join(" "));

    // Execute the command
    let status = unpack_command.args(unpack_args).status()?;
    if !status.success() {
        anyhow::bail!("Failed to unpack ffmpeg ({})", extension);
    }

    // Move binaries
    let move_bin = |path: &Path| {
        let file_name = binary_folder.join(
            path
                .file_name()
                .with_context(||
                    format!("Path {} does not have a file_name", path.to_string_lossy())
                )?
        );
        if path.exists() {
            rename(path, &file_name)?;
        } else {
            println!("Expected binary not found: {:?}", path);
            return Err(anyhow::anyhow!("Binary not found: {:?}", path));
        }
        Ok(())
    };

    // Adjust paths for Windows and Unix-like systems
    let (ffmpeg_bin, ffprobe_bin) = if cfg!(target_os = "windows") {
        ("ffmpeg.exe", "ffprobe.exe")
    } else {
        ("ffmpeg", "ffprobe")
    };

    let ffmpeg_path = temp_folder.join(ffmpeg_bin);
    let ffprobe_path = temp_folder.join(ffprobe_bin);

    move_bin(&ffmpeg_path)?;
    move_bin(&ffprobe_path)?;

    // Delete archive and unpacked files
    if temp_folder.exists() && temp_folder.is_dir() {
        println!("Removing temp folder {:?}", temp_folder);
        remove_dir_all(&temp_folder)?;
    } else {
        println!("Temp folder not found or not a directory: {:?}", temp_folder);
    }

    if from_archive.exists() {
        println!("Removing archive {:?}", from_archive);
        remove_file(from_archive)?;
    } else {
        println!("Archive file not found: {:?}", from_archive);
    }
    Ok(())
}
