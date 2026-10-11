//! Random and damaged input for every parser of bytes from the network:
//! none may panic (debug builds stop on overflows too) or take long.
use std::net::{IpAddr, SocketAddr};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant, SystemTime};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn bytes(&mut self, seeds: &[Vec<u8>]) -> Vec<u8> {
        let r = self.next();
        if !seeds.is_empty() && !r.is_multiple_of(3) {
            let mut v = seeds[(r as usize / 3) % seeds.len()].clone();
            let flips = 1 + self.next() % 8;
            for _ in 0..flips {
                if v.is_empty() {
                    break;
                }
                let i = (self.next() as usize) % v.len();
                match self.next() % 4 {
                    0 => v[i] = self.next() as u8,
                    1 => v[i] = [0, 0xff, 0x7f, 0x80, 0x81, 0x84][(self.next() % 6) as usize],
                    2 => v.truncate(i),
                    _ => {
                        let b = self.next() as u8;
                        v.insert(i, b)
                    }
                }
            }
            v
        } else {
            let n = (self.next() % 700) as usize;
            (0..n).map(|_| self.next() as u8).collect()
        }
    }
}

fn run(name: &str, seeds: Vec<Vec<u8>>, iters: u32, f: impl Fn(&[u8])) {
    let mut rng = Rng(0x9e3779b97f4a7c15 ^ name.len() as u64);
    let start = Instant::now();
    let mut slowest = Duration::ZERO;
    for _ in 0..iters {
        let input = rng.bytes(&seeds);
        let t = Instant::now();
        if catch_unwind(AssertUnwindSafe(|| f(&input))).is_err() {
            let hex: String = input.iter().map(|b| format!("{b:02x}")).collect();
            panic!("{name} panicked on {hex}");
        }
        slowest = slowest.max(t.elapsed());
    }
    eprintln!("{name}: {iters} ok in {:?}, slowest {:?}", start.elapsed(), slowest);
}

#[test]
fn fuzz_parsers() {
    let ip: IpAddr = [10, 0, 0, 1].into();
    let now = SystemTime::now();
    let snmp_seed = vec![
        0x30, 0x29, 0x02, 0x01, 0x01, 0x04, 0x06, b'p', b'u', b'b', b'l', b'i', b'c', 0xa2, 0x1c, 0x02, 0x02, 0x12,
        0x34, 0x02, 0x01, 0x00, 0x02, 0x01, 0x00, 0x30, 0x10, 0x30, 0x0e, 0x06, 0x08, 0x2b, 0x06, 0x01, 0x02, 0x01,
        0x01, 0x05, 0x00, 0x04, 0x02, b's', b'w',
    ];
    run("snmp::decode", vec![snmp_seed.clone()], 20_000, |b| {
        let _ = netmgr::snmp::decode(b);
    });
    let mut trap = snmp_seed.clone();
    trap[13] = 0xa7;
    run(
        "snmp::decode_trap",
        vec![trap.clone(), {
            let mut t = trap.clone();
            t[13] = 0xa6;
            t
        }],
        20_000,
        |b| {
            let _ = netmgr::snmp::decode_trap(b, ip);
        },
    );
    let mut dhcp = vec![0u8; 240];
    dhcp[0] = 1;
    dhcp[1] = 1;
    dhcp[2] = 6;
    dhcp[236..240].copy_from_slice(&[99, 130, 83, 99]);
    dhcp.extend_from_slice(&[53, 1, 1, 12, 3, b'a', b'b', b'c', 55, 2, 1, 3, 255]);
    run("dhcpd::parse", vec![dhcp.clone()], 20_000, |b| {
        let _ = netmgr::dhcpd::parse(b);
    });
    let mut offer = dhcp.clone();
    offer[0] = 2;
    run("dhcp::parse_offer", vec![offer], 20_000, |b| {
        let _ = netmgr::dhcp::parse_offer(b, 0);
    });
    let dns = vec![
        0x12, 0x34, 0x81, 0x80, 0, 1, 0, 1, 0, 0, 0, 0, 7, b'e', b'x', b'a', b'm', b'p', b'l', b'e', 3, b'c', b'o',
        b'm', 0, 0, 1, 0, 1, 0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 93, 184, 216, 34,
    ];
    run("dns::parse_reply", vec![dns], 20_000, |b| {
        let _ = netmgr::dns::parse_reply(b);
    });
    let mut ntp = vec![0u8; 48];
    ntp[0] = 0x24;
    ntp[1] = 2;
    let ntp_req = {
        let mut r = vec![0u8; 48];
        r[0] = 0x23;
        r
    };
    let server: SocketAddr = "10.0.0.1:123".parse().unwrap();
    run("ntp::parse", vec![ntp], 20_000, |b| {
        let _ = netmgr::ntp::parse(server, b, now, now);
    });
    run("ntp::answer", vec![ntp_req], 20_000, |b| {
        let _ = netmgr::ntp::answer(b, now, now, 3);
    });
    let lldp = {
        let mut f = vec![0x01, 0x80, 0xc2, 0, 0, 0x0e, 0, 1, 2, 3, 4, 5, 0x88, 0xcc];
        f.extend_from_slice(&[
            0x02, 0x07, 0x04, 0, 1, 2, 3, 4, 5, 0x04, 0x05, 0x05, b'G', b'i', b'1', b'/', 0x06, 0x02, 0, 120, 0x0a,
            0x03, b's', b'w', b'1', 0, 0,
        ]);
        f
    };
    let cdp = {
        let mut f = vec![
            0x01, 0x00, 0x0c, 0xcc, 0xcc, 0xcc, 0, 1, 2, 3, 4, 5, 0, 30, 0xaa, 0xaa, 0x03, 0, 0, 0x0c, 0x20, 0, 2, 180,
            0, 0,
        ];
        f.extend_from_slice(&[0, 1, 0, 7, b's', b'w', b'2', 0, 3, 0, 8, b'G', b'i', b'0', b'/', 0, 0x0a, 0, 6, 0, 10]);
        f
    };
    run("discovery::parse_frame", vec![lldp.clone(), cdp.clone()], 20_000, |b| {
        let _ = netmgr::discovery::parse_frame(b);
    });
    run("discovery::pcapng_frames", vec![lldp.clone()], 10_000, |b| {
        let _ = netmgr::discovery::pcapng_frames(b);
    });
    let eth = {
        let mut f = vec![0xff; 6];
        f.extend_from_slice(&[0, 0x11, 0x22, 0x33, 0x44, 0x55, 0x08, 0x00]);
        f.extend_from_slice(&[0x45, 0, 0, 40, 0, 0, 0x40, 0, 64, 6, 0, 0, 10, 0, 0, 1, 10, 0, 0, 2]);
        f.extend_from_slice(&[0, 80, 0x1f, 0x90, 0, 0, 0, 1, 0, 0, 0, 0, 0x50, 0x02, 0xff, 0xff, 0, 0, 0, 0]);
        f
    };
    for link in [1u32, 0, 101, 113, 228, 229, 12] {
        run(&format!("capture::decode link {link}"), vec![eth.clone(), lldp.clone(), cdp.clone()], 10_000, |b| {
            let _ = netmgr::capture::decode(link, b);
        });
    }
    let dir = std::env::temp_dir().join(format!("netmgr-fuzz-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let pcap = {
        let mut v = vec![0xd4, 0xc3, 0xb2, 0xa1, 2, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 0, 0, 1, 0, 0, 0];
        v.extend(netmgr::capture::pcap_record(1, 2, &eth, eth.len()));
        v
    };
    let path = dir.join("f.pcap");
    run("capture::read_file", vec![pcap], 2_000, |b| {
        std::fs::write(&path, b).unwrap();
        let _ = netmgr::capture::read_file(&path);
    });
    let _ = std::fs::remove_dir_all(&dir);
    run(
        "servers::parse_request",
        vec![b"\x00\x01fw.bin\x00octet\x00blksize\x001468\x00tsize\x000\x00".to_vec()],
        20_000,
        |b| {
            let _ = netmgr::servers::parse_request(b);
        },
    );
    run(
        "servers::parse_syslog",
        vec![b"<34>1 2025-10-11T22:14:15.003Z sw1 su - ID47 [x a=\"1\"] hello".to_vec()],
        20_000,
        |b| {
            let _ = netmgr::servers::parse_syslog(b, ip);
        },
    );
    run("servers::split_tcp_syslog", vec![b"12 <13>message\n<14>x\n".to_vec()], 20_000, |b| {
        let _ = netmgr::servers::split_tcp_syslog(b);
    });
    run(
        "backup::clean",
        vec!["\x1b[6n\x1b]0;t\x07hostname \x1b[1msw1\x1b[0m\n\x1b[99999999999999999999C!\nend\n".as_bytes().to_vec()],
        10_000,
        |b| {
            let _ = netmgr::backup::clean(&String::from_utf8_lossy(b));
        },
    );
    run("cmd::text", vec!["Ответ от 8.8.8.8: время=15мс TTL=117".as_bytes().to_vec()], 10_000, |b| {
        let _ = netmgr::cmd::text(b);
        let _ = netmgr::tools::parse_ping_line(&String::from_utf8_lossy(b));
    });
}
