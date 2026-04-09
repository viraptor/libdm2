use libdm2::lzvn;

/// Classify LZVN opcodes by type for analysis
fn classify_opcodes(data: &[u8]) -> Vec<(&'static str, usize)> {
    let mut ops = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let opc = data[i];
        match opc {
            0x06 => { ops.push(("eos", i)); break; }
            0x0E | 0x16 => { ops.push(("nop", i)); i += 1; }
            0xE0 => {
                if i + 1 >= data.len() { break; }
                let l = data[i + 1] as usize + 16;
                ops.push(("lrg_l", i));
                i += 2 + l;
            }
            0xE1..=0xEF => {
                let l = (opc & 0x0F) as usize;
                ops.push(("sml_l", i));
                i += 1 + l;
            }
            0xF0 => {
                ops.push(("lrg_m", i));
                i += 2;
            }
            0xF1..=0xFF => {
                ops.push(("sml_m", i));
                i += 1;
            }
            _ if (opc & 0x07) == 6 && opc >= 0x40 => {
                // pre_d: LLMMM110 + L literal bytes
                let l = (opc >> 6) as usize;
                ops.push(("pre_d", i));
                i += 1 + l;
            }
            _ if (opc & 0x07) == 7 => {
                // lrg_d: LLMMM111 DD DD + L literal bytes
                let l = (opc >> 6) as usize;
                ops.push(("lrg_d", i));
                i += 3 + l;
            }
            _ if opc >= 0xA0 && opc <= 0xBF => {
                // med_d: 3 bytes + L literal bytes
                let l = ((opc >> 3) & 3) as usize;
                ops.push(("med_d", i));
                i += 3 + l;
            }
            _ if (opc & 0x07) <= 5 => {
                // sml_d: 2 bytes + L literal bytes
                let l = (opc >> 6) as usize;
                ops.push(("sml_d", i));
                i += 2 + l;
            }
            _ => {
                ops.push(("???", i));
                break;
            }
        }
    }
    ops
}

#[test]
fn analyze_encoder_output() {
    // Test cases matching the crosscheck failures
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("9 bytes zeros", vec![0u8; 9]),
        ("9 bytes seq", (0u8..9).collect()),
        ("16 bytes seq", (0u8..16).collect()),
        ("32 bytes seq", (0u8..32).collect()),
        ("100 bytes gradient", (0..100).map(|i: u32| (i * 7 + 13) as u8).collect()),
        ("255 bytes gradient", (0..255).map(|i: u32| (i * 4) as u8).collect()),
        ("256 bytes zeros", vec![0u8; 256]),
        ("1024 bytes pattern", (0..1024).map(|i: u32| (i * 7 + 13) as u8).collect()),
    ];

    for (name, data) in &cases {
        let encoded = lzvn::encode(data);
        let ops = classify_opcodes(&encoded);

        let mut op_counts = std::collections::HashMap::new();
        for (op, _) in &ops {
            *op_counts.entry(*op).or_insert(0) += 1;
        }

        let mut counts: Vec<_> = op_counts.iter().collect();
        counts.sort_by_key(|(name, _)| *name);

        eprintln!("{}: {} -> {} bytes, opcodes: {:?}",
            name, data.len(), encoded.len(), counts);

        // Verify our own decode works
        let mut decoded = vec![0u8; data.len()];
        let n = lzvn::decode(&encoded, &mut decoded).unwrap();
        assert_eq!(n, data.len());
        assert_eq!(&decoded, data.as_slice(), "self-roundtrip failed for {name}");
    }
}
