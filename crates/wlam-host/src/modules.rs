//! Host-provided DLL modules and callable exports in the compiled bundle.
use std::collections::BTreeMap;
#[derive(Debug)]
struct Module {
    references: u32,
    exports: BTreeMap<String, u32>,
}
#[derive(Debug)]
pub struct Modules {
    handles: crate::objects::Handles<Module>,
    names: BTreeMap<String, u32>,
    imports: BTreeMap<u32, (String, String)>,
}
fn canonical(name: &str) -> String {
    let name = name.to_ascii_lowercase();
    if name.ends_with(".dll") {
        name
    } else {
        format!("{name}.dll")
    }
}
impl Default for Modules {
    fn default() -> Self {
        let mut result = Self {
            handles: crate::objects::Handles::with_namespace(5),
            names: BTreeMap::new(),
            imports: BTreeMap::new(),
        };
        for name in [
            "kernel32.dll",
            "user32.dll",
            "gdi32.dll",
            "opengl32.dll",
            "glu32.dll",
        ] {
            let handle = result
                .handles
                .insert(Module {
                    references: 0,
                    exports: BTreeMap::new(),
                })
                .unwrap();
            result.names.insert(name.into(), handle);
        }
        result
    }
}
impl Modules {
    pub fn handle(&self, name: &str) -> Option<u32> {
        self.names.get(&canonical(name)).copied()
    }
    pub fn load(&mut self, name: &str) -> Option<u32> {
        let handle = self.handle(name)?;
        let module = self.handles.get_mut(handle)?;
        module.references = module.references.checked_add(1)?;
        Some(handle)
    }
    pub fn free(&mut self, handle: u32) -> bool {
        let Some(module) = self.handles.get_mut(handle) else {
            return false;
        };
        let Some(count) = module.references.checked_sub(1) else {
            return false;
        };
        module.references = count;
        true // Builtin modules remain resident, including their static imports.
    }
    pub fn register(&mut self, dll: &str, name: &str, address: u32) {
        if let Some(handle) = self.handle(dll) {
            self.handles
                .get_mut(handle)
                .unwrap()
                .exports
                .insert(name.into(), address);
            self.imports.insert(address,(dll.to_ascii_lowercase(),name.to_owned()));
        }
    }
    pub fn import_at(&self,address:u32)->Option<(String,String)>{self.imports.get(&address).cloned()}
    pub fn procedure(&self, handle: u32, name: &str) -> Option<u32> {
        self.handles.get(handle)?.exports.get(name).copied()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn builtin_names_references_and_exports_are_checked() {
        let mut modules = Modules::default();
        let handle = modules.load("KERNEL32").unwrap();
        assert_eq!(modules.load("kernel32.dll"), Some(handle));
        modules.register("kernel32", "GetACP", 0x4ca7b0);
        assert_eq!(modules.procedure(handle, "GetACP"), Some(0x4ca7b0));
        assert_eq!(modules.procedure(handle, "getacp"), None);
        assert_eq!(modules.load("dashres.dll"), None);
        assert!(modules.free(handle));
        assert!(modules.free(handle));
        assert!(!modules.free(handle));
        assert!(!modules.free(0));
    }
}
