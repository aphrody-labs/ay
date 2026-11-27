//! The natives under js/crypto.ts (RustCrypto, getrandom), against the vectors of their RFCs and what
//! JavaScriptCore Bun answers.

use boa_engine::js_string;
use boa_engine::property::Attribute;
use bun_js::JSGlobalObject;

fn check(cases: &[(&str, &str)]) {
    JSGlobalObject::with_new(|global| {
        {
            // SAFETY: one operation on the context, before any script runs.
            let context = unsafe { global.context() };
            let natives = bun_runtime_js::crypto::natives(context);
            context
                .register_global_property(js_string!("n"), natives, Attribute::all())
                .expect("n");
        }
        global
            .evaluate_script(
                "globalThis.hex = u => u === null ? 'null' : Array.from(u, b => b.toString(16).padStart(2, '0')).join('');\
                 globalThis.utf8 = s => Uint8Array.from(s, c => c.charCodeAt(0));\
                 globalThis.unhex = h => Uint8Array.from(h.match(/../g) ?? [], p => parseInt(p, 16));",
            )
            .expect("helpers");
        let mut failures = Vec::new();
        for &(source, expected) in cases {
            let actual = match global.evaluate_script(&format!("String((() => {{ {source} }})())"))
            {
                Ok(value) => value.to_utf8(global).expect("a string"),
                Err(error) => format!(
                    "throws {}",
                    global
                        .take_exception(error)
                        .to_utf8(global)
                        .unwrap_or_default()
                ),
            };
            if actual != expected {
                failures.push(format!(
                    "{source}\n    expected: {expected}\n    actual:   {actual}"
                ));
            }
        }
        assert!(
            failures.is_empty(),
            "{} of {} differ:\n{}",
            failures.len(),
            cases.len(),
            failures.join("\n")
        );
    });
}

#[test]
fn digests_and_hmacs() {
    check(&[
        (
            "return hex(n.digest('sha256', utf8('abc')))",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        ),
        (
            "return hex(n.digest('SHA-1', utf8('abc')))",
            "a9993e364706816aba3e25717850c26c9cd0d89d",
        ),
        (
            "return hex(n.digest('md5', utf8('')))",
            "d41d8cd98f00b204e9800998ecf8427e",
        ),
        (
            "return hex(n.digest('sha3-256', utf8('')))",
            "a7ffc6f8bf1ed76651c14756a061d662f580ff4de43b49fa82d80a4b80f8434a",
        ),
        (
            "return hex(n.digest('shake128', utf8('')))",
            "7f9c2ba4e88f827d616045507605853e",
        ),
        (
            "return hex(n.digest('shake256', utf8(''), 8))",
            "46b9dd2b0ba88d13",
        ),
        ("return hex(n.digest('nope', utf8('')))", "null"),
        (
            "return n.algorithms().length + ' ' + n.canonicalName('SHA-512/256')",
            "19 sha512-256",
        ),
        (
            "return hex(n.hmac('sha256', unhex('0b'.repeat(20)), utf8('Hi There')))",
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
        ),
        (
            "return hex(n.hmac('shake128', utf8('k'), utf8('')))",
            "null",
        ),
        (
            "const h = n.hasherCreate('sha256'); n.hasherUpdate(h, utf8('a')); const c = n.hasherCopy(h); n.hasherUpdate(c, utf8('bc')); return hex(n.hasherDigest(c)) + ' ' + n.hasherInfo(h)",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad sha256,32,false",
        ),
        (
            "const h = n.hasherCreate('sha1', utf8('key')); n.hasherDigest(h); return [n.hasherUpdate(h, utf8('')), n.hasherDigest(h), n.hasherCreate('shake256', utf8('k'))].join()",
            "false,,false",
        ),
    ]);
}

#[test]
fn key_derivations() {
    check(&[
        (
            "return hex(n.pbkdf2('sha1', utf8('password'), utf8('salt'), 2, 20))",
            "ea6c014dc72d6f8ccd1ed92ace1d41f0d8de8957",
        ),
        (
            "return hex(n.hkdf('sha256', unhex('0b'.repeat(22)), unhex('000102030405060708090a0b0c'), unhex('f0f1f2f3f4f5f6f7f8f9'), 42))",
            "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865",
        ),
        (
            "return hex(n.scrypt(utf8(''), utf8(''), 16, 1, 1, 64))",
            "77d6576238657b203b19ca42c18a0497f16b4844e3074ae8dfdffa3fede21442fcd0069ded0948f8326a753a0fc81f17e8d3e0fb2e0d3628cf35e20c38d18906",
        ),
        (
            "return hex(n.scrypt(utf8(''), utf8(''), 15, 1, 1, 64))",
            "null",
        ),
        (
            "return [n.timingSafeEqual(utf8('ab'), utf8('ab')), n.timingSafeEqual(utf8('ab'), utf8('ac'))].join()",
            "true,false",
        ),
    ]);
}

#[test]
fn uuids_random_and_aes_gcm() {
    check(&[
        (
            "return n.uuidString(n.uuidV5(utf8('hello.example.com'), 'dns'))",
            "fdda765f-fc57-5604-a269-52a7df8164ec",
        ),
        (
            "return n.uuidString(n.uuidV5(utf8('http://example.com/hello'), 'URL'))",
            "3bbcee75-cecc-5b56-8031-b6641c1ed1f1",
        ),
        (
            "return n.uuidV5(utf8('x'), 'zyxwvuts-rqpo-nmlk-jihg-fedcba000000')",
            "null",
        ),
        (
            "return /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(n.uuidV4())",
            "true",
        ),
        (
            "const u = n.uuidString(n.uuidV7(0x0123456789ab)); return u.slice(0, 15)",
            "01234567-89ab-7",
        ),
        (
            "const a = n.uuidString(n.uuidV7()), b = n.uuidString(n.uuidV7()); return a < b",
            "true",
        ),
        ("return n.randomBytes(5).length", "5"),
        (
            "const k = new Uint8Array(16), iv = new Uint8Array(12); return hex(n.aesGcm(true, k, iv, new Uint8Array(0), new Uint8Array(0), 16))",
            "58e2fccefa7e3061367f1d57a4e7455a",
        ),
        (
            "const k = new Uint8Array(32), iv = new Uint8Array(12); const c = n.aesGcm(true, k, iv, utf8('hi'), utf8('ad'), 16); c[0] ^= 1; return [hex(n.aesGcm(false, k, iv, (c[0] ^= 1, c), utf8('ad'), 16)), n.aesGcm(false, k, iv, c, utf8('xx'), 16)].join()",
            "6869,",
        ),
    ]);
}

#[test]
fn bun_hash() {
    check(&[
        ("return n.hash('adler32', utf8('hello'), '0')", "103547413"),
        ("return n.hash('crc32', utf8('hello'), '0')", "907060870"),
        (
            "return typeof n.hash('wyhash', utf8('hello'), '0')",
            "bigint",
        ),
    ]);
}
