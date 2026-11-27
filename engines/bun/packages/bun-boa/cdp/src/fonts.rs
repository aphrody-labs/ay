//! Embedded fonts: no system font discovery (fontconfig is C), so every document gets the same
//! collection of OFL faces, fetched into `vendor/fonts` by `scripts/setup.ts`.

use std::sync::Arc;

use blitz_dom::util::{SvgGenericSlot, register_svg_font};
use linebender_resource_handle::Blob;
use parley::FontContext;
use parley::fontique::{Collection, CollectionOptions, FamilyId, GenericFamily, Script, SourceCache};

macro_rules! font {
    ($file:literal) => {
        include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../vendor/fonts/", $file)) as &'static [u8]
    };
}

const SANS: &[&[u8]] = &[font!("Arimo.ttf"), font!("Arimo-Italic.ttf")];
const SERIF: &[&[u8]] = &[font!("Tinos-Regular.ttf"), font!("Tinos-Bold.ttf"), font!("Tinos-Italic.ttf")];
const MONO: &[&[u8]] = &[font!("NotoSansMono.ttf")];
const FALLBACK: &[&[u8]] = &[font!("NotoSans.ttf"), font!("NotoSansSymbols2.ttf")];

fn register(collection: &mut Collection, faces: &[&'static [u8]], svg_slot: Option<SvgGenericSlot>) -> Vec<FamilyId> {
    let mut ids = Vec::new();
    for (i, bytes) in faces.iter().enumerate() {
        let blob = Blob::new(Arc::new(*bytes));
        if i == 0 {
            register_svg_font(Arc::new(*bytes), svg_slot);
        }
        for (family, _) in collection.register_fonts(blob, None) {
            if !ids.contains(&family) {
                ids.push(family);
            }
        }
    }
    ids
}

fn build() -> FontContext {
    let mut collection = Collection::new(CollectionOptions { shared: true, system_fonts: false });
    let sans = register(&mut collection, SANS, Some(SvgGenericSlot::SansSerif));
    let serif = register(&mut collection, SERIF, Some(SvgGenericSlot::Serif));
    let mono = register(&mut collection, MONO, Some(SvgGenericSlot::Monospace));
    let fallback = register(&mut collection, FALLBACK, None);

    for generic in [GenericFamily::SansSerif, GenericFamily::SystemUi, GenericFamily::UiSansSerif, GenericFamily::Cursive, GenericFamily::Fantasy] {
        collection.set_generic_families(generic, sans.iter().copied());
    }
    for generic in [GenericFamily::Serif, GenericFamily::UiSerif] {
        collection.set_generic_families(generic, serif.iter().copied());
    }
    for generic in [GenericFamily::Monospace, GenericFamily::UiMonospace] {
        collection.set_generic_families(generic, mono.iter().copied());
    }
    let chain: Vec<FamilyId> = sans.iter().chain(&fallback).chain(&serif).chain(&mono).copied().collect();
    collection.set_fallbacks(Script::COMMON, chain.iter().copied());
    collection.set_fallbacks(Script::from_bytes(*b"Latn"), chain.iter().copied());
    FontContext { collection, source_cache: SourceCache::new_shared() }
}

thread_local! {
    static CONTEXT: FontContext = build();
}

/// A font context sharing the embedded collection (cheap: the collection is shared).
pub fn context() -> FontContext {
    CONTEXT.with(FontContext::clone)
}
