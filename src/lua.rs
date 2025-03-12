use std::ops::Deref;

use gc_arena::{DynamicRootSet, Rootable};
use mlua::prelude::*;

use crate::xmltree::dom::unsize_node;

mod debug;
pub mod io;
mod util;
mod xml;

type LuaArena = gc_arena::Arena<Rootable![DynamicRootSet<'_>]>;

trait LuaExt {
    fn gc(&self) -> mlua::AppDataRef<LuaArena>;
    fn protect_table(&self, table: &LuaTable) -> LuaResult<()>;
    fn create_protected_table(&self) -> LuaResult<LuaTable>;
    fn create_overlay_table(&self, lower: &LuaTable) -> LuaResult<LuaTable>;
}

impl LuaExt for Lua {
    fn gc(&self) -> mlua::AppDataRef<LuaArena> {
        self.app_data_ref::<LuaArena>()
            .expect("lua object should contain a dynamic gc arena")
    }

    fn protect_table(&self, table: &LuaTable) -> LuaResult<()> {
        let metatable = self.create_table()?;

        let cloned = table.clone();
        metatable.raw_set(
            "__index",
            self.create_function(move |_, (_, key): (LuaValue, LuaValue)| cloned.raw_get::<LuaValue>(key))?,
        )?;
        metatable.raw_set(
            "__newindex",
            self.create_function(|_, _: ()| Err::<(), _>(LuaError::runtime("attempt to update a protected table")))?,
        )?;
        metatable.raw_set("__metatable", LuaValue::Boolean(true))?;

        table.set_metatable(Some(metatable));

        Ok(())
    }

    fn create_protected_table(&self) -> LuaResult<LuaTable> {
        let table = self.create_table()?;
        self.protect_table(&table)?;
        Ok(table)
    }

    fn create_overlay_table(&self, lower: &LuaTable) -> LuaResult<LuaTable> {
        let upper = self.create_table()?;
        let metatable = self.create_table()?;
        metatable.raw_set("__index", lower)?;

        let upper_clone = upper.clone();
        // NOTE: The table parameter is intentionally ignore to avoid providing
        //       a "raw_set on anything primitive".
        metatable.raw_set(
            "__newindex",
            self.create_function(move |_, (_t, k, v): (LuaTable, LuaValue, LuaValue)| upper_clone.raw_set(k, v))?,
        )?;

        metatable.raw_set("__metatable", LuaValue::Boolean(true))?;

        upper.set_metatable(Some(metatable));

        Ok(upper)
    }
}

pub struct ModLuaRuntime {
    lua: Lua,
    lib_table: LuaTable,
}

pub struct LuaContext {
    pub document_root: Option<xml::DynamicElement>,
    pub print_arena_stats: bool,
}

macro_rules! load_builtin_lib {
    ($lua: expr, $filename: literal) => {
        $lua.load(include_str!(concat!("lua/", $filename)))
            .set_name(concat!("<BUILTIN>/", $filename))
            .exec()
            .context(concat!("Failed to execute builtin ", $filename, " script"))?;
    };
}

impl ModLuaRuntime {
    pub fn new() -> LuaResult<Self> {
        let lua = mlua::Lua::new_with(
            mlua::StdLib::TABLE | mlua::StdLib::STRING | mlua::StdLib::MATH | mlua::StdLib::PACKAGE,
            mlua::LuaOptions::new(),
        )
        .context("Failed to initialize Lua")?;

        lua.globals().raw_remove("dofile")?;
        lua.globals().raw_remove("collectgarbage")?;
        lua.globals().raw_remove("loadfile")?;
        // While this could potentially be useful, it bypasses
        // protected metatables so for now it's disabled.
        lua.globals().raw_remove("rawset")?;
        lua.protect_table(&lua.globals().raw_get::<LuaTable>("string")?)?;
        lua.protect_table(&lua.globals().raw_get::<LuaTable>("table")?)?;
        lua.protect_table(&lua.globals().raw_get::<LuaTable>("math")?)?;
        Self::setup_package(&lua)?;
        lua.protect_table(&lua.globals().raw_get::<LuaTable>("package")?)?;
        // This is replaced by the script environment table later.
        lua.globals().raw_remove("_G")?;

        // This function causes HRTB deduction problems so no, I cannot replace the closure.
        #[allow(clippy::redundant_closure)]
        let arena: LuaArena = LuaArena::new(|mc| DynamicRootSet::new(mc));

        lua.set_app_data(arena);

        let lib_table = lua.create_table()?;
        lua.globals().raw_set("mod", lib_table.clone())?;        

        load_builtin_lib!(lua, "util.lua");
        load_builtin_lib!(lua, "iterutil.lua");
        load_builtin_lib!(lua, "table.lua");
        load_builtin_lib!(lua, "debug.lua");

        for result in lib_table.pairs() {
            let (_, value): (LuaValue, LuaValue) = result?;
            if let Some(table) = value.as_table() {
                lua.protect_table(table)?;
            }
        }

        lib_table.raw_set(
            "xml",
            xml::create_xml_lib(&lua).context("Failed to create xml library table")?,
        )?;

        debug::extend_debug_library(&lua, lib_table.get::<LuaTable>("debug")?)
            .context("Failed to load debug builtins")?;

        util::extend_util_library(&lua, lib_table.get::<LuaTable>("util")?).context("Failed to load util builtins")?;

        lua.protect_table(&lib_table)
            .context("Failed to make builtin mod table read-only")?;

        Ok(Self { lua, lib_table })
    }

    fn setup_package(lua: &Lua) -> LuaResult<()> {
        const REQUIRE_PATH: &str = "./?.lua;/data/?.lua;/?.lua";
        let package = lua.globals().raw_get::<LuaTable>("package")?;

        package.raw_remove("cpath")?;
        package.raw_remove("loadlib")?;
        package.raw_remove("preload")?;
        package.raw_remove("searchpath")?;
        package.raw_set("path", REQUIRE_PATH)?;

        let searchers = package.raw_get::<LuaTable>("searchers")?;
        searchers.clear()?;
        load_builtin_lib!(lua, "vfssearcher.lua");

        Ok(())
    }

    pub fn arena(&self) -> impl Deref<Target = LuaArena> + use<'_> {
        self.lua.gc()
    }

    pub fn with_filesystems<'a, R>(
        &self,
        iter: impl IntoIterator<Item = (impl IntoLua, &'a mut (dyn io::LuaFS + 'a))>,
        scoped: impl FnOnce() -> LuaResult<R>,
    ) -> LuaResult<R> {
        self.lua.scope(|scope| {
            let vfs = self.lua.create_protected_table()?;
            for (name, fs) in iter {
                vfs.raw_set(name, scope.create_userdata(fs)?)?;
            }
            self.lib_table.raw_set("vfs", vfs)?;

            let result = scoped();

            self.lib_table.raw_remove("vfs")?;

            result
        })
    }

    pub fn run(&self, code: &str, chunk_name: &str, context: &mut LuaContext) -> LuaResult<()> {
        let lua = &self.lua;

        let env = lua.create_overlay_table(&lua.globals())?;
        env.raw_set("_G", &env)?;

        if let Some(ref root) = context.document_root {
            env.set(
                "document",
                lua.create_userdata(xml::LuaDocument {
                    root: xml::LuaElement(root.clone()),
                })?,
            )?;
        }

        lua.load(code)
            .set_name(chunk_name)
            .set_mode(mlua::ChunkMode::Text)
            .set_environment(env)
            .exec()?;

        if context.print_arena_stats {
            let mut gc = lua.app_data_mut::<LuaArena>().unwrap();
            println!("allocated bytes: {:?}", gc.metrics().total_allocation());
            println!("allocated bytes (gc only): {:?}", gc.metrics().total_gc_allocation());
            println!("debt: {:?}", gc.metrics().allocation_debt());
            gc.collect_all();
            println!(
                "allocated bytes after collection (gc only): {:?}",
                gc.metrics().total_gc_allocation()
            );
            println!(
                "allocated bytes after collection: {:?}",
                gc.metrics().total_allocation()
            );
        }

        Ok(())
    }
}
