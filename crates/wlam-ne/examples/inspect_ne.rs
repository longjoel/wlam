use std::env;
use std::fs;

use wlam_ne::{NeImage, NeResourceIdentifier, NeResourceManager};

fn identifier(value: &NeResourceIdentifier) -> String {
    match value {
        NeResourceIdentifier::Ordinal(value) => format!("#{value}"),
        NeResourceIdentifier::Name(value) => String::from_utf8_lossy(value).into_owned(),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut paths = env::args_os().skip(1).peekable();
    if paths.peek().is_none() {
        return Err("usage: cargo run -p wlam-ne --example inspect_ne -- <image.ne> [...]".into());
    }

    for path in paths {
        let path = std::path::PathBuf::from(path);
        let bytes = fs::read(&path)?;
        let image = NeImage::parse(&bytes)?;
        let mut loaded = image.load_segments(&bytes, 0x100)?;
        let mut resources = NeResourceManager::default();
        println!(
            "{}: {} segments, {} resources",
            path.display(),
            image.segments.len(),
            image.resources.len()
        );
        for resource in &image.resources {
            let data = image.resource_data(&bytes, resource)?;
            let info = resources
                .find_resource(&image, &resource.resource_type, &resource.name)?
                .ok_or("parsed resource could not be found")?;
            let first_load = resources
                .load_resource(&image, &bytes, &mut loaded, info)?
                .ok_or("resource load failed")?;
            let shared_load = resources
                .load_resource(&image, &bytes, &mut loaded, info)?
                .ok_or("repeated resource load failed")?;
            if first_load != shared_load {
                return Err("repeated resource loads did not share a global handle".into());
            }
            let first_lock = resources
                .lock_resource(&loaded, first_load)?
                .ok_or("resource lock failed")?;
            let second_lock = resources
                .lock_resource(&loaded, first_load)?
                .ok_or("repeated resource lock failed")?;
            if first_lock != second_lock {
                return Err("repeated resource locks returned different far pointers".into());
            }
            let guest_data = loaded.read(
                first_lock.selector,
                u32::from(first_lock.offset),
                resource.size,
                wlam_ne::NeAccess::Read,
            )?;
            if guest_data != data {
                return Err("loaded resource bytes differ from the image payload".into());
            }
            let remains_locked = resources.unlock_resource(first_load);
            let fully_unlocked = resources.unlock_resource(first_load);
            let first_free = resources.free_resource(&mut loaded, first_load)?;
            let final_free = resources.free_resource(&mut loaded, first_load)?;
            if !remains_locked || fully_unlocked || !first_free || !final_free {
                return Err("resource lock or reference counts were inconsistent".into());
            }
            println!(
                "  {} {}: file {:#x}, {} bytes, flags {:#06x}, HRSRC {:#06x}, HGLOBAL {:#06x}, far {}:{:04x}",
                identifier(&resource.resource_type),
                identifier(&resource.name),
                resource.file_offset,
                data.len(),
                resource.flags,
                info.0,
                first_load.0,
                first_lock.selector,
                first_lock.offset,
            );
        }
    }
    Ok(())
}
