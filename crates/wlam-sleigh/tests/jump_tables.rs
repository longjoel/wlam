//! Discover and execute a mask-bounded table, like the CRT memcpy tail.
use std::collections::{BTreeMap,BTreeSet};
use wlam_harness::wasmi::{Engine,Module,Store,Linker};
use wlam_pe::{Pe,PeKind,Machine,Section,ResourceTable};
#[test]
fn indexed_jump_loads_a_guest_address_and_dispatches_to_its_case() {
    let mut code=vec![0xcc;0x60];
    code[..10].copy_from_slice(&[0x83,0xe0,3,0xff,0x24,0x85,0,0x20,0x40,0]);
    let targets=(1..=4).map(|i|0x401000u64+i*16).collect::<Vec<_>>();
    for i in 1..=4 {
        let at=i*16;code[at..at+6].copy_from_slice(&[0xc7,0x05,0,0x30,0,0]);
        code[at+6..at+10].copy_from_slice(&(i as u32*11).to_le_bytes());code[at+10]=0xc3;
    }
    let section=|name:&str,rva,data:Vec<u8>,flags|Section {name:name.into(),virtual_address:rva,virtual_size:data.len() as u32,file_offset:0,file_size:data.len() as u32,characteristics:flags,data};
    let pe=Pe {
        kind:PeKind::Pe32,machine:Machine::I386,image_base:0x400000,entry_point_rva:0x1000,
        section_alignment:4096,file_alignment:512,subsystem:2,dll_characteristics:0,size_of_image:0x3000,size_of_headers:512,
        sections:vec![section(".text",0x1000,code,0x60000020),section(".rdata",0x2000,targets.iter().flat_map(|t|(*t as u32).to_le_bytes()).collect(),0x40000040)],
        directories:vec![],imports:vec![],exports:vec![],relocations:vec![],resources:ResourceTable::default(),unwind_info_rva:0,unwind_info_size:0,raw:vec![],
    };
    let (blocks,discovery)=wlam_sleigh::with_big_stack(move|| {
        let discovery=wlam_sleigh::discover::discover(&pe,16);let mut blocks=BTreeMap::new();
        for b in discovery.blocks() {
            let rva=(b.start-pe.image_base) as u32;let section=&pe.sections[pe.section_of(rva).unwrap()];
            let lifted=wlam_sleigh::lift_block(&section.data[(rva-section.virtual_address) as usize..],b.start);
            blocks.insert(b.start,wlam_sleigh::emit::convert(&lifted));
        }
        (blocks,discovery)
    });
    assert_eq!(discovery.indirect_targets().get(&0x401000),Some(&targets));
    let mut registers=wlam_emit::seeded_registers();let mut used=BTreeSet::new();
    for b in blocks.values() {for op in &b.ops {wlam_sleigh::emit::collect_regs(op,&mut used);}}
    for r in used {if !registers.contains(&r) {registers.push(r);}}
    let program=wlam_emit::chain::partition_with_indirect_targets(&[0x401000],&blocks,&registers,discovery.indirect_targets());
    let bytes=wlam_emit::chain::build_module_with_memory(&program,&blocks,80).unwrap();
    let engine=Engine::default();let module=Module::new(&engine,&bytes[..]).unwrap();
    for index in 0u32..=4 {
        let mut store=Store::new(&engine,());
        let instance=Linker::new(&engine).instantiate(&mut store,&module).unwrap().start(&mut store).unwrap();
        let memory=instance.get_memory(&store,"memory").unwrap();
        for (i,target) in targets.iter().enumerate() {memory.write(&mut store,0x402000+i*4,&(*target as u32).to_le_bytes()).unwrap();}
        for (name,value) in [("EAX",index),("ESP",0x4000)] {
            let i=registers.iter().position(|r|r==name).unwrap();memory.write(&mut store,wlam_emit::snapshot_address(i) as usize,&value.to_le_bytes()).unwrap();
        }
        instance.get_typed_func::<(),()>(&store,"entry").unwrap().call(&mut store,()).unwrap();
        assert_eq!(u32::from_le_bytes(memory.data(&store)[0x3000..0x3004].try_into().unwrap()),(index%4+1)*11);
    }
}
