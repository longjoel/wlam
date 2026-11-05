//! ISA assertions with sentinels: an IR interpreter alone would share a
//! decoder's incorrect ordering of pointer updates and memory accesses.
use std::collections::{BTreeMap,BTreeSet};
use wlam_harness::wasmi::{Engine,Module,Store,Linker};

fn run(code:&[u8],registers:&[(&str,u32)],initial:&[(u32,Vec<u8>)])->(Vec<u8>,BTreeMap<String,u32>) {
    let code=code.to_vec();
    let lifted=wlam_sleigh::with_big_stack(move||wlam_sleigh::lift_block(&code,0x1000));
    let mut block=wlam_sleigh::emit::convert(&lifted);
    block.terminator=wlam_emit::ir::Terminator::Return;
    let mut names=wlam_emit::seeded_registers();let mut used=BTreeSet::new();
    for op in &block.ops {wlam_sleigh::emit::collect_regs(op,&mut used);}
    for name in used {if !names.contains(&name) {names.push(name);}}
    let blocks=BTreeMap::from([(0x1000,block)]);
    let program=wlam_emit::chain::partition(&[0x1000],&blocks,&names);
    let bytes=wlam_emit::chain::build_module_with_memory(&program,&blocks,16).unwrap();
    let engine=Engine::default();let module=Module::new(&engine,&bytes[..]).unwrap();
    let mut store=Store::new(&engine,());
    let instance=Linker::new(&engine).instantiate(&mut store,&module).unwrap().start(&mut store).unwrap();
    let memory=instance.get_memory(&store,"memory").unwrap();
    for (address,bytes) in initial {memory.write(&mut store,*address as usize,bytes).unwrap();}
    for (name,value) in registers {
        let index=names.iter().position(|n|n==name).unwrap();
        memory.write(&mut store,wlam_emit::snapshot_address(index) as usize,&value.to_le_bytes()).unwrap();
    }
    instance.get_typed_func::<(),()>(&store,"entry").unwrap().call(&mut store,()).unwrap();
    let data=memory.data(&store).to_vec();
    let state=names.iter().enumerate().map(|(i,name)| {
        let at=wlam_emit::snapshot_address(i) as usize;
        (name.clone(),u32::from_le_bytes(data[at..at+4].try_into().unwrap()))
    }).collect();
    (data,state)
}

#[test]
fn rep_stosd_writes_exactly_the_requested_words_in_both_directions() {
    for (direction,start,end) in [(0,0x2004,0x2018),(1,0x2014,0x2000)] {
        let (data,state)=run(&[0xf3,0xab],&[("EAX",0x11223344),("ECX",5),("EDI",start),("DF",direction)],&[(0x2000,vec![0xa5;28])]);
        assert_eq!(&data[0x2000..0x2004],&[0xa5;4]);
        assert_eq!(&data[0x2018..0x201c],&[0xa5;4]);
        for word in data[0x2004..0x2018].chunks_exact(4) {assert_eq!(word,0x11223344u32.to_le_bytes());}
        assert_eq!(state["ECX"],0);assert_eq!(state["EDI"],end);
    }
}

#[test]
fn rep_movsd_copies_from_the_original_addresses_and_preserves_sentinels() {
    for (direction,src,dst,src_end,dst_end) in [(0,0x2000,0x3004,0x200c,0x3010),(1,0x2008,0x300c,0x1ffc,0x3000)] {
        let source=(1u32..=3).flat_map(u32::to_le_bytes).collect::<Vec<_>>();
        let (data,state)=run(&[0xf3,0xa5],&[("ECX",3),("ESI",src),("EDI",dst),("DF",direction)],&[(0x2000,source.clone()),(0x3000,vec![0xa5;20])]);
        assert_eq!(&data[0x3004..0x3010],source);
        assert_eq!(&data[0x3000..0x3004],&[0xa5;4]);assert_eq!(&data[0x3010..0x3014],&[0xa5;4]);
        assert_eq!(state["ESI"],src_end);assert_eq!(state["EDI"],dst_end);assert_eq!(state["ECX"],0);
    }
}
