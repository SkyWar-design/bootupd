use std::io::prelude::*;
use std::path::Path;
use std::process::Command;
use std::fs;

use crate::component::*;
use crate::model::*;
use crate::packagesystem;
use anyhow::{bail, Result};
use crate::util;
use serde::{Deserialize, Serialize};

// grub-install file path
pub(crate) const GRUB_BIN: &str = "usr/sbin/grub-install";

#[derive(Serialize, Deserialize, Debug)]
struct BlockDevice {
    path: String,
    pttype: Option<String>,
    parttypename: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
struct Devices {
    blockdevices: Vec<BlockDevice>,
}

#[derive(Default)]
pub(crate) struct Bios {}

impl Bios {
    // Get target device for running update
    fn get_device(&self) -> Result<String> {
        let mut cmd: Command;
        #[cfg(target_arch = "x86_64")]
        {
            // Find /boot partition
            cmd = Command::new("findmnt");
            cmd.arg("--noheadings")
                .arg("--nofsroot")
                .arg("--output")
                .arg("SOURCE")
                .arg("/boot");
            let partition = util::cmd_output(&mut cmd)?;

            // Use lsblk to find parent device
            cmd = Command::new("lsblk");
            cmd.arg("--paths")
                .arg("--noheadings")
                .arg("--output")
                .arg("PKNAME")
                .arg(partition.trim());
        }

        #[cfg(target_arch = "powerpc64")]
        {
            // Get PowerPC-PReP-boot partition
            cmd = Command::new("realpath");
            cmd.arg("/dev/disk/by-partlabel/PowerPC-PReP-boot");
        }

        let device = util::cmd_output(&mut cmd)?;
        Ok(device)
    }

    // Returns `true` if grub modules are installed
    fn check_grub_modules(&self) -> Result<bool> {
        let usr_path = Path::new("/usr/lib64/grub");
        #[cfg(target_arch = "x86_64")]
        {
            usr_path.join("i386-pc").try_exists().map_err(Into::into)
        }
        #[cfg(target_arch = "powerpc64")]
        {
            usr_path
                .join("powerpc-ieee1275")
                .try_exists()
                .map_err(Into::into)
        }
    }

    // Run grub-install
    fn run_grub_install(&self, dest_root: &str, device: &str) -> Result<()> {
        if !self.check_grub_modules()? {
            bail!("Failed to find grub modules");
        }
        let grub_install = Path::new("/").join(GRUB_BIN);
        if !grub_install.exists() {
            bail!("Failed to find {:?}", grub_install);
        }

        let mut cmd = Command::new(grub_install);
        let boot_dir = Path::new(dest_root).join("boot");
        // Forcibly add mdraid1x and part_gpt
        #[cfg(target_arch = "x86_64")]
        cmd.args(["--target", "i386-pc"])
            .args(["--boot-directory", boot_dir.to_str().unwrap()])
            .args(["--modules", "mdraid1x part_gpt"])
            .arg(device);

        #[cfg(target_arch = "powerpc64")]
        cmd.args(&["--target", "powerpc-ieee1275"])
            .args(&["--boot-directory", boot_dir.to_str().unwrap()])
            .arg("--no-nvram")
            .arg(device);

        let cmdout = cmd.output()?;
        if !cmdout.status.success() {
            std::io::stderr().write_all(&cmdout.stderr)?;
            bail!("Failed to run {:?}", cmd);
        }

        #[cfg(target_arch = "x86_64")]
        {
            let source = Path::new("/usr/lib64/grub/x86_64-efi");
            let destination = boot_dir.join("grub").join("x86_64-efi");

            // Check if source directory exists
            if !source.exists() {
                bail!("Source directory {:?} not found", source);
            }

            // Perform copying
            copy_dir_all(&source, &destination)?;
            log::info!("Directory {:?} successfully copied to {:?}", source, destination);
        }

        #[cfg(target_arch = "powerpc64")]
        {
            let source = Path::new("/usr/lib64/grub/powerpc-ieee1275");
            let destination = boot_dir.join("powerpc-ieee1275");

            // Check if source directory exists
            if !source.exists() {
                bail!("Source directory {:?} not found", source);
            }

            // Perform copying
            copy_dir_all(&source, &destination)?;
            log::info!("Directory {:?} successfully copied to {:?}", source, destination);
        }

        Ok(())
    }

    // Check bios_boot partition on gpt type disk
    fn get_bios_boot_partition(&self) -> Result<Option<String>> {
        let target = self.get_device()?;
        // Use lsblk to list children with bios_boot
        let output = Command::new("lsblk")
            .args([
                "--json",
                "--output",
                "PATH,PTTYPE,PARTTYPENAME",
                target.trim(),
            ])
            .output()?;
        if !output.status.success() {
            std::io::stderr().write_all(&output.stderr)?;
            bail!("Failed to run lsblk");
        }

        let output = String::from_utf8(output.stdout)?;
        // Deserialize JSON string into Devices struct
        let Ok(devices) = serde_json::from_str::<Devices>(&output) else {
            bail!("Could not deserialize JSON output from lsblk");
        };

        // Find device with parttypename "BIOS boot"
        for device in devices.blockdevices {
            if let Some(parttypename) = &device.parttypename {
                if parttypename == "BIOS boot" && device.pttype.as_deref() == Some("gpt") {
                    return Ok(Some(device.path));
                }
            }
        }
        Ok(None)
    }
}

/// Recursive directory copy function
fn copy_dir_all(src: &Path, dest: &Path) -> Result<()> {
    if !src.exists() {
        bail!("Directory {:?} not found", src);
    }

    fs::create_dir_all(dest)?;

    for entry_result in fs::read_dir(src)? {
        let entry = entry_result?;
        let file_type = entry.file_type()?;
        let src_path = entry.path();
        let dest_path = dest.join(entry.file_name());

        if file_type.is_dir() {
            copy_dir_all(&src_path, &dest_path)?;
        } else if file_type.is_file() {
            fs::copy(&src_path, &dest_path)?;
        } else {
            // Handle other file types (symlinks, etc.) if necessary
            log::warn!("Warning: Unsupported file type: {:?}", src_path);
        }
    }

    Ok(())
}

impl Component for Bios {
    fn name(&self) -> &'static str {
        "BIOS"
    }

    fn install(
        &self,
        src_root: &openat::Dir,
        dest_root: &str,
        device: &str,
        _update_firmware: bool,
    ) -> Result<InstalledContent> {
        let Some(meta) = get_component_update(src_root, self)? else {
            anyhow::bail!("Update metadata for component {} not found", self.name());
        };

        self.run_grub_install(dest_root, device)?;
        Ok(InstalledContent {
            meta,
            filetree: None,
            adopted_from: None,
        })
    }

    fn generate_update_metadata(&self, sysroot_path: &str) -> Result<ContentMetadata> {
        let grub_install = Path::new(sysroot_path).join(GRUB_BIN);
        if !grub_install.exists() {
            bail!("Failed to find {:?}", grub_install);
        }

        // Query the rpm database and get package and build time information for /usr/sbin/grub-install
        let meta = packagesystem::query_files(sysroot_path, [&grub_install])?;
        write_update_metadata(sysroot_path, self, &meta)?;
        Ok(meta)
    }

    fn query_adopt(&self) -> Result<Option<Adoptable>> {
        #[cfg(target_arch = "x86_64")]
        if crate::efi::is_efi_booted()? && self.get_bios_boot_partition()?.is_none() {
            log::debug!("Skipping adopt BIOS");
            return Ok(None);
        }
        crate::component::query_adopt_state()
    }

    fn adopt_update(&self, _: &openat::Dir, update: &ContentMetadata) -> Result<InstalledContent> {
        let Some(meta) = self.query_adopt()? else {
            anyhow::bail!("Failed to find adoptable system")
        };

        let device = self.get_device()?;
        let device = device.trim();
        self.run_grub_install("/", device)?;
        Ok(InstalledContent {
            meta: update.clone(),
            filetree: None,
            adopted_from: Some(meta.version),
        })
    }

    fn query_update(&self, sysroot: &openat::Dir) -> Result<Option<ContentMetadata>> {
        get_component_update(sysroot, self)
    }

    fn run_update(&self, sysroot: &openat::Dir, _: &InstalledContent) -> Result<InstalledContent> {
        let updatemeta = self.query_update(sysroot)?.expect("update available");
        let device = self.get_device()?;
        let device = device.trim();
        self.run_grub_install("/", device)?;

        let adopted_from = None;
        Ok(InstalledContent {
            meta: updatemeta,
            filetree: None,
            adopted_from,
        })
    }

    fn validate(&self, _: &InstalledContent) -> Result<ValidationResult> {
        Ok(ValidationResult::Skip)
    }

    fn get_efi_vendor(&self, _: &openat::Dir) -> Result<Option<String>> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use std::fs::{self, File};
    use std::io::Write;

    #[test]
    fn test_deserialize_lsblk_output() {
        let data = include_str!("../tests/fixtures/example-lsblk-output.json");
        let devices: Devices = serde_json::from_str(&data)
            .expect("JSON was not well-formatted");
        assert_eq!(devices.blockdevices.len(), 7);
        assert_eq!(devices.blockdevices[0].path, "/dev/sr0");
        assert!(devices.blockdevices[0].pttype.is_none());
        assert!(devices.blockdevices[0].parttypename.is_none());
    }

    #[test]
    fn test_copy_dir_all() -> Result<()> {
        let src_dir = tempdir()?;
        let dest_dir = tempdir()?;

        // Create directory and file structure in src_dir
        let sub_dir = src_dir.path().join("subdir");
        fs::create_dir(&sub_dir)?;
        let file_path = sub_dir.join("testfile.txt");
        let mut file = File::create(&file_path)?;
        writeln!(file, "Hello, world!")?;

        // Perform copying
        copy_dir_all(src_dir.path(), &dest_dir.path().join("copied_subdir"))?;

        // Verify that files are copied
        let copied_file_path = dest_dir.path().join("copied_subdir").join("subdir").join("testfile.txt");
        assert!(copied_file_path.exists());

        let content = fs::read_to_string(copied_file_path)?;
        assert_eq!(content.trim(), "Hello, world!");

        Ok(())
    }

    #[test]
    fn test_copy_dir_all_nonexistent_src() {
        let src = Path::new("/nonexistent/source");
        let dest = Path::new("/nonexistent/dest");
        let result = copy_dir_all(src, dest);
        assert!(result.is_err());
    }
}
