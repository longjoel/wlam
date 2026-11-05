//! Run a compiled image against implemented kernel APIs, refusing every gap.
//! cargo run -p wlam-sleigh --example probe_image -- image.exe bundle-directory
use std::sync::{Arc,Mutex};
use wlam_harness::{instantiate,load_image,run_entry_diagnosing,seed_register,Host,IatPolicy};
fn main() -> Result<(),String> {
    let args=std::env::args().skip(1).collect::<Vec<_>>();
    if args.len()!=2 {return Err("usage: probe_image image.exe bundle-directory".into());}
    let image=std::fs::read(&args[0]).map_err(|e|e.to_string())?;
    let pe=wlam_pe::Pe::parse(&image).map_err(|e|e.to_string())?;
    let module=std::fs::read(std::path::Path::new(&args[1]).join("guest.wasm")).map_err(|e|e.to_string())?;
    let mut host=Host::new();wlam_host::kernel32(&mut host);
    let host=Arc::new(Mutex::new(host));
    let (mut store,instance)=instantiate(&module,host.clone()).map_err(|e|e.to_string())?;
    store.set_fuel(2_000_000).map_err(|e|e.to_string())?;
    let memory=instance.get_memory(&store,"memory").ok_or("module has no guest memory")?;
    load_image(memory.data_mut(&mut store),&pe,IatPolicy::Poison).map_err(|e|e.to_string())?;
    let mut seed=Vec::new();seed_register(&mut seed,4,0x70000);seed_register(&mut seed,10,0x60000);seed.push((0x60000,u32::MAX));
    let outcome=run_entry_diagnosing(&mut store,&instance,&seed,&host);
    println!("OUTCOME: {outcome:?}");
    if let Err(wlam_harness::RunError::Refused(diagnostic))=&outcome {
        let data=memory.data(&store);let start=diagnostic.args_ptr as u32 as usize;
        let arguments=(0..6).filter_map(|i|data.get(start+i*4..start+i*4+4).map(|bytes|u32::from_le_bytes(bytes.try_into().unwrap()))).collect::<Vec<_>>();
        println!("REFUSAL_ARGS: {arguments:x?}");
    }
    let host=host.lock().unwrap_or_else(|error|error.into_inner());
    for (api,count) in host.call_counts().iter().filter(|(_,count)|**count>0){println!("REACHED: {api} {count}");}
    let data=memory.data(&store);
    // A refusal has no guest PC attached to it: the register snapshot is only
    // written when the guest spills. The live guest stack at the refused call
    // is then the only evidence of which guest code made it, so print it.
    if let Err(wlam_harness::RunError::Refused(diagnostic))=&outcome {
        let start=diagnostic.args_ptr as usize;
        let words=(0..12).map(|i|{let o=start+i*4;u32::from_le_bytes(data.get(o..o+4).and_then(|b|b.try_into().ok()).unwrap_or([0;4]))}).collect::<Vec<_>>();
        println!("REFUSED_ARGS {start:#010x}: {words:08x?}");
    }
    for (index,name) in wlam_emit::GUEST_LOCALS.iter().enumerate(){let at=wlam_emit::snapshot_address(index) as usize;println!("REGISTER: {name}={:#010x}",u32::from_le_bytes(data[at..at+4].try_into().unwrap()));}
    // On an anonymous trap the register snapshot may not include the faulting
    // PC, so dump the stack around the last known ESP: return addresses and
    // saved frames are usually enough to place the failure in guest code.
    let esp_index=wlam_emit::GUEST_LOCALS.iter().position(|n|*n=="ESP").unwrap_or(4);
    let esp_at=wlam_emit::snapshot_address(esp_index) as usize;
    let esp=u32::from_le_bytes(data[esp_at..esp_at+4].try_into().unwrap()) as usize;
    for row in 0..16 {
        let base=esp.saturating_sub(0x40)+row*16;
        let words=(0..4).map(|i|{let o=base+i*4;format!("{:08x}",u32::from_le_bytes(data[o..o+4].try_into().unwrap()))}).collect::<Vec<_>>();
        println!("STACK {base:#010x}: {}",words.join(" "));
    }
    Ok(())
}
