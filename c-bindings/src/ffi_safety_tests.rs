//! FFI safety audit tests.
//!
//! These drive the exported C API the way a C caller would, and are meant to
//! be run under valgrind as well as natively:
//!
//! ```text
//! cargo valgrind test -p logos-blockchain-c -- --test-threads=1 ffi_audit
//! ```
//!
//! Every allocation handed out by the API is released through the matching
//! `free_*` function, so anything valgrind reports as definitely lost is a
//! leak in the bindings rather than in the tests.
//!
//! The tests in [`crashers`] are `#[ignore]`d: each one demonstrates a way to
//! take the host process down and has to be run on its own.

use std::{
    ffi::{CStr, CString, c_char},
    path::{Path, PathBuf},
    ptr,
    sync::atomic::{AtomicPtr, AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use lb_c_macros::panic_to_error;
use lb_node::UserConfig;
use lb_utils::yaml::{OnUnknownKeys, deserialize_value_at_path};
use serial_test::serial;
use tempfile::TempDir;

use crate::{
    LogosBlockchainNode, OperationStatus, OperationStatusCode,
    api::{
        blend::{blend_info, blend_join_as_core_node},
        chain::get_chain_id,
        channel::get_channel_state,
        config::{
            GenerateConfigArgs, MergeConfigFlags, generate_user_config, merge_user_config,
            migrate_user_config, migrate_user_config_0_1_2, participate, update_user_config,
        },
        cryptarchia::{free_cryptarchia_info, get_block_events, get_cryptarchia_info},
        deployment::{free_deployment_info, get_deployment_info},
        free_cstring, free_operation_status,
        keys::{KeyType, add_key, generate_key, remove_key},
        leader::leader_claim,
        lifecycle::{shutdown_node, start_lb_node},
        network::get_network_info,
        peer::get_peer_id,
        pow::{
            PoWClaimableRewards, PoWStatus, free_pow_claimable_rewards, free_pow_status, pow_claim,
            pow_claimable_rewards, pow_start_auto_claim, pow_start_mining, pow_status,
            pow_stop_auto_claim, pow_stop_mining,
        },
        storage::{get_block, get_blocks, get_transaction},
        subscriptions::{
            subscribe_to_lib_blocks, subscribe_to_new_blocks, subscribe_to_processed_blocks,
        },
        time::{free_time_info, get_time_info},
        types::{
            claimable_vouchers::ClaimableVouchers, known_addresses::KnownAddresses,
            leader_aged_notes::LeaderAgedNotes, wallet_notes::WalletNotes,
        },
        version::get_build_version_info,
        wallet::{
            ChannelDepositArguments, ChannelDepositWithNotesArguments, TransferFundsArguments,
            channel_deposit, channel_deposit_with_notes, free_claimable_vouchers,
            free_known_addresses, free_leader_aged_notes, free_wallet_notes, get_balance,
            get_claimable_vouchers, get_known_addresses, get_leader_aged_notes, get_wallet_notes,
            submit_signed_transaction, transfer_funds, wallet_fund_tx,
        },
    },
    result::FfiResult,
    return_error_if_null_pointer,
};

trait IntoStatus {
    fn into_status(self) -> OperationStatus;
}

impl IntoStatus for OperationStatus {
    fn into_status(self) -> OperationStatus {
        self
    }
}

impl<Value> IntoStatus for FfiResult<Value, OperationStatus> {
    fn into_status(self) -> OperationStatus {
        self.error
    }
}

/// Releases the status the way a well-behaved C caller would and returns its
/// code and message.
fn consume(status: impl IntoStatus) -> (OperationStatusCode, String) {
    let status = status.into_status();
    let code = status.code;
    let message = if status.message.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(status.message) }
            .to_string_lossy()
            .into_owned()
    };
    unsafe { free_operation_status(status) };
    (code, message)
}

/// For values that may come from an error result: the free either succeeds or
/// reports the null pointer. The status is released either way.
fn freed(status: OperationStatus) {
    assert!(matches!(
        code(status),
        OperationStatusCode::Ok | OperationStatusCode::NullPointer
    ));
}

fn code(status: impl IntoStatus) -> OperationStatusCode {
    consume(status).0
}

fn cstring(path: &Path) -> CString {
    CString::new(path.to_string_lossy().as_bytes()).expect("Path should not contain NUL")
}

fn node_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("Crate has a parent directory")
        .join("nodes/node")
}

/// An isolated copy of the standalone node and deployment configs.
struct TestConfigPaths {
    temp_dir: TempDir,
    node_config: CString,
    deployment_config: CString,
}

impl TestConfigPaths {
    fn new() -> Self {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        std::fs::create_dir_all(temp_dir.path().join("state/logs"))
            .expect("Failed to create log dir");

        let node_config_path = temp_dir.path().join("standalone-node-config.yaml");
        let deployment_config_path = temp_dir.path().join("standalone-deployment-config.yaml");

        let mut node_config = deserialize_value_at_path::<UserConfig>(
            &node_dir().join("standalone-node-config.yaml"),
            OnUnknownKeys::Fail,
        )
        .expect("Standalone user config should deserialize");
        node_config.state.base_folder = temp_dir.path().join("state");
        node_config.api.backend.listen_address = "127.0.0.1:0"
            .parse()
            .expect("Local address should be correct");
        std::fs::write(
            &node_config_path,
            serde_yaml::to_string(&node_config).expect("Config should serialize"),
        )
        .expect("Failed to write node config");
        std::fs::copy(
            node_dir().join("standalone-deployment-config.yaml"),
            &deployment_config_path,
        )
        .expect("Failed to copy deployment config");

        Self {
            node_config: cstring(&node_config_path),
            deployment_config: cstring(&deployment_config_path),
            temp_dir,
        }
    }

    fn start(&self) -> *mut LogosBlockchainNode {
        let result =
            unsafe { start_lb_node(self.node_config.as_ptr(), self.deployment_config.as_ptr()) };
        let node = result.value;
        let (status, message) = consume(result);
        assert_eq!(status, OperationStatusCode::Ok, "start failed: {message}");
        assert!(!node.is_null());
        node
    }
}

const NO_INSERT: MergeConfigFlags = MergeConfigFlags {
    source_insert_missing: false,
    extra_insert_missing: false,
};

mod no_node {
    use super::*;

    /// Every exported function that takes a required pointer must reject NULL
    /// with a `NullPointer` status instead of dereferencing it.
    #[test]
    fn null_pointers_are_rejected() {
        let node: *const LogosBlockchainNode = ptr::null();
        let id = [0u8; 32];
        let s = c"x".as_ptr();
        let null_s: *const c_char = ptr::null();
        let np = OperationStatusCode::NullPointer;

        unsafe {
            assert_eq!(code(get_chain_id(node)), np);
            assert_eq!(code(leader_claim(node)), np);
            assert_eq!(code(get_network_info(node)), np);
            assert_eq!(code(get_channel_state(node, id.as_ptr())), np);
            assert_eq!(code(get_time_info(node)), np);
            assert_eq!(code(get_cryptarchia_info(node)), np);
            assert_eq!(code(get_block_events(node, &raw const id)), np);
            assert_eq!(code(get_block(node, &raw const id)), np);
            assert_eq!(code(get_transaction(node, &raw const id)), np);
            assert_eq!(code(get_blocks(node, 0, 1)), np);
            assert_eq!(code(subscribe_to_new_blocks(node, noop_callback)), np);
            assert_eq!(code(subscribe_to_processed_blocks(node, noop_callback)), np);
            assert_eq!(code(subscribe_to_lib_blocks(node, noop_callback)), np);
            assert_eq!(code(get_known_addresses(node)), np);
            assert_eq!(code(get_claimable_vouchers(node, ptr::null())), np);
            assert_eq!(code(get_balance(node, id.as_ptr(), ptr::null())), np);
            assert_eq!(code(get_wallet_notes(node, id.as_ptr(), ptr::null())), np);
            assert_eq!(code(get_leader_aged_notes(node, ptr::null())), np);
            assert_eq!(code(transfer_funds(node, ptr::null())), np);
            assert_eq!(code(channel_deposit_with_notes(node, ptr::null())), np);
            assert_eq!(code(channel_deposit(node, ptr::null())), np);
            assert_eq!(code(wallet_fund_tx(node, s)), np);
            assert_eq!(code(submit_signed_transaction(node, s)), np);
            assert_eq!(code(pow_start_mining(node)), np);
            assert_eq!(code(pow_stop_mining(node)), np);
            assert_eq!(code(pow_start_auto_claim(node)), np);
            assert_eq!(code(pow_stop_auto_claim(node)), np);
            assert_eq!(code(pow_claim(node, ptr::null())), np);
            assert_eq!(code(pow_claimable_rewards(node)), np);
            assert_eq!(code(pow_status(node)), np);
            assert_eq!(code(blend_join_as_core_node(node, s, id.as_ptr())), np);
            assert_eq!(code(blend_info(node)), np);
            assert_eq!(code(shutdown_node(ptr::null_mut())), np);

            assert_eq!(code(start_lb_node(null_s, null_s)), np);
            assert_eq!(code(start_lb_node(null_s, s)), np);
            assert_eq!(code(get_peer_id(null_s)), np);
            assert_eq!(code(update_user_config(null_s, s)), np);
            assert_eq!(code(update_user_config(s, null_s)), np);
            assert_eq!(code(migrate_user_config(null_s, s)), np);
            assert_eq!(code(migrate_user_config(s, null_s)), np);
            assert_eq!(code(migrate_user_config_0_1_2(null_s, s, s)), np);
            assert_eq!(code(migrate_user_config_0_1_2(s, null_s, s)), np);
            assert_eq!(code(migrate_user_config_0_1_2(s, s, null_s)), np);
            assert_eq!(code(merge_user_config(null_s, s, null_s, NO_INSERT)), np);
            assert_eq!(code(merge_user_config(s, null_s, null_s, NO_INSERT)), np);
            assert_eq!(code(participate(null_s, s, s, null_s)), np);
            assert_eq!(code(participate(s, null_s, s, null_s)), np);
            assert_eq!(code(participate(s, s, null_s, null_s)), np);
            assert_eq!(code(generate_key(null_s, s, KeyType::Zk, null_s)), np);
            assert_eq!(code(generate_key(s, null_s, KeyType::Zk, null_s)), np);
            assert_eq!(code(add_key(null_s, s, KeyType::Zk, s, null_s)), np);
            assert_eq!(code(add_key(s, null_s, KeyType::Zk, s, null_s)), np);
            assert_eq!(code(add_key(s, s, KeyType::Zk, null_s, null_s)), np);
            assert_eq!(code(remove_key(null_s, s, s)), np);
            assert_eq!(code(remove_key(s, null_s, s)), np);
            assert_eq!(code(remove_key(s, s, null_s)), np);
            assert_eq!(code(get_deployment_info(null_s, null_s)), np);
        }
    }

    unsafe extern "C" fn noop_callback(_data: *const c_char) {}

    /// Every `free_*` reports a null pointer (the value an *error* result
    /// carries) as a `NullPointer` error with a message, and that status is
    /// released like any other.
    #[test]
    fn free_functions_report_null() {
        let statuses = unsafe {
            [
                free_cstring(ptr::null_mut()),
                free_time_info(ptr::null_mut()),
                free_cryptarchia_info(ptr::null_mut()),
                free_deployment_info(ptr::null_mut()),
                free_known_addresses(KnownAddresses::default()),
                free_claimable_vouchers(ClaimableVouchers::default()),
                free_pow_claimable_rewards(PoWClaimableRewards::default()),
                free_wallet_notes(WalletNotes::default()),
                free_leader_aged_notes(LeaderAgedNotes::default()),
                free_pow_status(PoWStatus::default()),
            ]
        };
        for status in statuses {
            let (code, message) = consume(status);
            assert_eq!(code, OperationStatusCode::NullPointer);
            assert!(message.contains("null"), "{message}");
        }
    }

    /// A null entry in the list must not keep the entries after it from being
    /// freed. Valgrind is what catches a regression here.
    #[test]
    fn free_known_addresses_skips_null_entries() {
        let entry = || Box::into_raw(Box::new([7u8; 32])).cast::<u8>();
        let entries: Box<[*mut u8]> = Box::new([entry(), ptr::null_mut(), entry()]);
        let len = entries.len();
        let addresses = KnownAddresses {
            addresses: Box::leak(entries).as_mut_ptr(),
            len,
        };
        assert!(unsafe { free_known_addresses(addresses) }.is_ok());
    }

    /// Any status can be handed to `free_operation_status`, whether or not it
    /// carries a message. Valgrind is what catches a regression here.
    #[test]
    fn free_operation_status_releases_any_status() {
        unsafe {
            free_operation_status(OperationStatus::OK);
            free_operation_status(free_cstring(ptr::null_mut()));
            free_operation_status(OperationStatus::error(
                OperationStatusCode::NotFound,
                "nope",
            ));
            // The `error` field of a result, on both outcomes.
            let result = get_build_version_info();
            assert!(free_cstring(result.value).is_ok());
            free_operation_status(result.error);
            free_operation_status(get_peer_id(ptr::null()).error);
        }
    }

    #[panic_to_error]
    extern "C" fn panics_with_status(message: *const c_char) -> OperationStatus {
        return_error_if_null_pointer!(message);
        panic!("{}", unsafe { CStr::from_ptr(message) }.to_string_lossy());
    }

    #[panic_to_error]
    extern "C" fn panics_with_result() -> FfiResult<*mut c_char, OperationStatus> {
        panic!("static message");
    }

    #[panic_to_error]
    extern "C" fn panics_with_unit() {
        std::panic::panic_any(42_u8);
    }

    /// A panic inside an exported function comes back as a `RuntimeError`
    /// instead of aborting the process, whatever the return type, and early
    /// returns inside the body still work.
    #[test]
    fn panics_become_errors() {
        assert_eq!(
            consume(panics_with_status(c"formatted".as_ptr())),
            (
                OperationStatusCode::RuntimeError,
                "Internal panic: formatted".into()
            )
        );
        assert_eq!(
            code(panics_with_status(ptr::null())),
            OperationStatusCode::NullPointer
        );

        let result = panics_with_result();
        assert!(result.value.is_null());
        assert_eq!(
            consume(result),
            (
                OperationStatusCode::RuntimeError,
                "Internal panic: static message".into()
            )
        );

        // Nothing to assert on: surviving the call is the test, and valgrind
        // checks the discarded status is released.
        panics_with_unit();
    }

    #[test]
    fn status_helpers() {
        let ok = OperationStatus::OK;
        assert!(ok.is_ok());
        assert!(!ok.is_error());
        let error = OperationStatus::error(OperationStatusCode::NotFound, "nope");
        assert!(error.is_error());
        assert_eq!(
            consume(error),
            (OperationStatusCode::NotFound, "nope".into())
        );
    }

    #[test]
    fn version_info_roundtrip() {
        let result = get_build_version_info();
        assert!(result.is_ok());
        let json = unsafe { CStr::from_ptr(result.value) }.to_str().unwrap();
        let _version: serde_json::Value = serde_json::from_str(json).expect("Version info is JSON");
        assert!(unsafe { free_cstring(result.value) }.is_ok());
    }

    #[test]
    fn deployment_info_roundtrip() {
        let paths = TestConfigPaths::new();
        let result = unsafe {
            get_deployment_info(paths.node_config.as_ptr(), paths.deployment_config.as_ptr())
        };
        let info = result.value;
        let (status, message) = consume(result);
        assert_eq!(status, OperationStatusCode::Ok, "{message}");

        let info_ref = unsafe { &*info };
        for pointer in [
            info_ref.chain_id,
            info_ref.node_version,
            info_ref.protocol_names.blend,
            info_ref.protocol_names.cryptarchia,
            info_ref.protocol_names.kademlia,
            info_ref.protocol_names.identify,
            info_ref.protocol_names.chain_sync,
            info_ref.protocol_names.mempool,
        ] {
            assert!(!unsafe { CStr::from_ptr(pointer) }.to_bytes().is_empty());
        }
        assert!(unsafe { free_deployment_info(info) }.is_ok());
    }

    /// `generate_user_config` through real pointers, including the odd ones: a
    /// null and an unparsable entry in the peer list.
    #[test]
    fn generate_user_config_through_pointers() {
        let temp_dir = TempDir::new().unwrap();
        let output = cstring(&temp_dir.path().join("user_config.yaml"));
        let kms = cstring(&temp_dir.path().join("keystore.yaml"));
        let state = cstring(&temp_dir.path().join("state"));
        let http = c"127.0.0.1:18080";
        let external = c"/ip4/203.0.113.7/udp/3000/quic-v1";
        let filter = c"info";
        let peer = c"/ip4/203.0.113.9/udp/3000/quic-v1";
        let garbage = c"not a multiaddr";
        let peers = [peer.as_ptr(), ptr::null(), garbage.as_ptr()];
        let count: u32 = 3;
        let net_port: u16 = 3123;
        let blend_port: u16 = 3124;
        let skip_ibd = true;

        let status = unsafe {
            generate_user_config(GenerateConfigArgs {
                initial_peers: peers.as_ptr(),
                initial_peers_count: &raw const count,
                output: output.as_ptr(),
                net_port: &raw const net_port,
                blend_port: &raw const blend_port,
                http_addr: http.as_ptr(),
                external_address: external.as_ptr(),
                state_path: state.as_ptr(),
                storage_path: ptr::null(),
                logs_path: ptr::null(),
                skip_ibd: &raw const skip_ibd,
                log_filter: filter.as_ptr(),
                kms_file: kms.as_ptr(),
            })
        };
        let (status, message) = consume(status);
        assert_eq!(status, OperationStatusCode::Ok, "{message}");
        assert!(temp_dir.path().join("user_config.yaml").exists());

        // The generated files feed the rest of the config/key API.
        let result = unsafe { get_peer_id(output.as_ptr()) };
        assert!(result.is_ok());
        assert!(unsafe { free_cstring(result.value) }.is_ok());

        let result =
            unsafe { generate_key(output.as_ptr(), kms.as_ptr(), KeyType::Zk, ptr::null()) };
        assert!(result.is_ok());
        assert!(unsafe { free_cstring(result.value) }.is_ok());

        let result = unsafe { get_deployment_info(output.as_ptr(), ptr::null()) };
        let info = result.value;
        let (status, message) = consume(result);
        assert_eq!(status, OperationStatusCode::Ok, "{message}");
        assert!(unsafe { free_deployment_info(info) }.is_ok());
    }

    /// Error paths allocate a message and nothing else; once the caller frees
    /// the message there must be nothing left behind.
    #[test]
    #[serial]
    fn error_paths_release_everything() {
        let temp_dir = TempDir::new().unwrap();
        let missing = cstring(&temp_dir.path().join("missing.yaml"));
        let not_utf8 = CString::new(vec![0xFF, 0xFE, b'/', b'x']).unwrap();
        let garbage_yaml_path = temp_dir.path().join("garbage.yaml");
        std::fs::write(&garbage_yaml_path, "a: [").unwrap();
        let garbage_yaml = cstring(&garbage_yaml_path);
        let paths = TestConfigPaths::new();

        unsafe {
            for path in [&missing, &not_utf8, &garbage_yaml] {
                let p = path.as_ptr();
                assert_ne!(code(start_lb_node(p, ptr::null())), OperationStatusCode::Ok);
                assert_ne!(
                    code(start_lb_node(paths.node_config.as_ptr(), p)),
                    OperationStatusCode::Ok
                );
                assert_ne!(
                    code(get_deployment_info(p, ptr::null())),
                    OperationStatusCode::Ok
                );
                assert_ne!(
                    code(get_deployment_info(paths.node_config.as_ptr(), p)),
                    OperationStatusCode::Ok
                );
                assert_ne!(code(get_peer_id(p)), OperationStatusCode::Ok);
                assert_ne!(code(update_user_config(p, p)), OperationStatusCode::Ok);
                assert_ne!(code(migrate_user_config(p, p)), OperationStatusCode::Ok);
                assert_ne!(
                    code(migrate_user_config_0_1_2(p, p, p)),
                    OperationStatusCode::Ok
                );
                assert_ne!(
                    code(merge_user_config(p, p, ptr::null(), NO_INSERT)),
                    OperationStatusCode::Ok
                );
                assert_ne!(
                    code(participate(p, p, p, ptr::null())),
                    OperationStatusCode::Ok
                );
                assert_ne!(
                    code(generate_key(p, p, KeyType::Ed25519, ptr::null())),
                    OperationStatusCode::Ok
                );
                assert_ne!(
                    code(remove_key(p, p, c"title".as_ptr())),
                    OperationStatusCode::Ok
                );
            }
            assert_eq!(
                code(participate(
                    missing.as_ptr(),
                    missing.as_ptr(),
                    missing.as_ptr(),
                    c"not-an-ip".as_ptr()
                )),
                OperationStatusCode::ValidationError
            );
            assert_eq!(
                code(add_key(
                    missing.as_ptr(),
                    missing.as_ptr(),
                    KeyType::Zk,
                    c"zz".as_ptr(),
                    ptr::null()
                )),
                OperationStatusCode::ValidationError
            );
            assert_eq!(
                code(add_key(
                    missing.as_ptr(),
                    missing.as_ptr(),
                    KeyType::Ed25519,
                    c"abcd".as_ptr(),
                    ptr::null()
                )),
                OperationStatusCode::ValidationError
            );
        }
    }

    /// An error message that echoes a NUL byte from its input must still
    /// reach the caller as a C string.
    #[test]
    #[serial]
    fn error_message_with_interior_nul() {
        let status = OperationStatus::error(OperationStatusCode::NotFound, "a\0b\0");
        assert_eq!(
            consume(status),
            (OperationStatusCode::NotFound, "a\\0b\\0".into())
        );

        // End to end: serde's "unknown variant" message echoes the offending
        // value verbatim, NUL included.
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.yaml");
        let yaml = std::fs::read_to_string(node_dir().join("standalone-node-config.yaml"))
            .unwrap()
            .replace("type: traversal", "type: \"a\\0b\"");
        std::fs::write(&path, yaml).unwrap();
        let path = cstring(&path);
        let (code, message) = consume(unsafe { start_lb_node(path.as_ptr(), ptr::null()) });
        assert_eq!(code, OperationStatusCode::InitializationError);
        assert!(message.contains("unknown variant `a\\0b`"), "{message}");
    }
}

mod with_node {
    use super::*;

    static NEW_BLOCKS: Counters = Counters::new();
    static PROCESSED: Counters = Counters::new();
    static LIB: Counters = Counters::new();

    struct Counters {
        events: AtomicUsize,
        bytes: AtomicUsize,
        sentinels: AtomicUsize,
    }

    impl Counters {
        const fn new() -> Self {
            Self {
                events: AtomicUsize::new(0),
                bytes: AtomicUsize::new(0),
                sentinels: AtomicUsize::new(0),
            }
        }

        fn record(&self, data: *const c_char) {
            if data.is_null() {
                self.sentinels.fetch_add(1, Ordering::SeqCst);
            } else {
                // Reads the whole string, so valgrind sees any invalid read.
                let len = unsafe { CStr::from_ptr(data) }.to_bytes().len();
                self.bytes.fetch_add(len, Ordering::SeqCst);
                self.events.fetch_add(1, Ordering::SeqCst);
            }
        }

        fn snapshot(&self) -> (usize, usize) {
            (
                self.events.load(Ordering::SeqCst),
                self.sentinels.load(Ordering::SeqCst),
            )
        }
    }

    unsafe extern "C" fn on_new_block(data: *const c_char) {
        NEW_BLOCKS.record(data);
    }
    unsafe extern "C" fn on_processed(data: *const c_char) {
        PROCESSED.record(data);
    }
    unsafe extern "C" fn on_lib(data: *const c_char) {
        LIB.record(data);
    }

    fn wait_secs() -> u64 {
        std::env::var("FFI_AUDIT_WAIT_SECS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(45)
    }

    /// Frees a C string result if it carries one and returns the status code.
    unsafe fn string_result(
        name: &str,
        result: FfiResult<*mut c_char, OperationStatus>,
    ) -> OperationStatusCode {
        let value = result.value;
        let (code, message) = consume(result);
        if code == OperationStatusCode::Ok {
            assert!(!value.is_null(), "{name}: ok with a null string");
            let len = unsafe { CStr::from_ptr(value) }.to_bytes().len();
            assert!(unsafe { free_cstring(value) }.is_ok());
            eprintln!("AUDIT {name}: Ok ({len} bytes)");
        } else {
            assert!(value.is_null(), "{name}: error with a non-null string");
            eprintln!("AUDIT {name}: {code:?} {message}");
        }
        code
    }

    fn log(name: &str, status: impl IntoStatus) -> OperationStatusCode {
        let (code, message) = consume(status);
        eprintln!("AUDIT {name}: {code:?} {message}");
        code
    }

    /// Starts a node, calls every node-bound function at least once on both
    /// the success and the error path, releases everything and shuts down.
    #[test]
    #[serial]
    fn full_api_sweep() {
        let paths = TestConfigPaths::new();
        let node = paths.start();
        let zero = [0u8; 32];
        // Not a canonical field element.
        let invalid_key = [0xFFu8; 32];

        unsafe {
            assert!(subscribe_to_new_blocks(node, on_new_block).is_ok());
            assert!(subscribe_to_processed_blocks(node, on_processed).is_ok());
            assert!(subscribe_to_lib_blocks(node, on_lib).is_ok());

            // ---- plain getters ----
            assert_eq!(
                string_result("get_chain_id", get_chain_id(node)),
                OperationStatusCode::Ok
            );

            let result = get_time_info(node);
            assert!(result.is_ok());
            assert!((*result.value).slot_duration_ms > 0);
            assert!(free_time_info(result.value).is_ok());

            let result = get_cryptarchia_info(node);
            assert!(result.is_ok());
            let tip = (*result.value).tip;
            assert!(free_cryptarchia_info(result.value).is_ok());

            let result = get_network_info(node);
            let n_peers = result.value.n_peers;
            log("get_network_info", result);
            assert_eq!(n_peers, 0);

            string_result("blend_info", blend_info(node));
            string_result("get_block(tip)", get_block(node, &raw const tip));
            assert_eq!(
                string_result("get_block(zero)", get_block(node, &raw const zero)),
                OperationStatusCode::NotFound
            );
            string_result(
                "get_block_events(tip)",
                get_block_events(node, &raw const tip),
            );
            string_result(
                "get_block_events(zero)",
                get_block_events(node, &raw const zero),
            );
            string_result(
                "get_transaction(zero)",
                get_transaction(node, &raw const zero),
            );
            string_result("get_blocks(0, 10)", get_blocks(node, 0, 10));
            string_result("get_blocks(10, 0)", get_blocks(node, 10, 0));
            string_result("get_blocks(0, MAX)", get_blocks(node, 0, u64::MAX));
            string_result(
                "get_channel_state(zero)",
                get_channel_state(node, zero.as_ptr()),
            );

            // ---- wallet ----
            let result = get_known_addresses(node);
            let FfiResult {
                value: addresses,
                error,
            } = result;
            assert_eq!(log("get_known_addresses", error), OperationStatusCode::Ok);
            eprintln!("AUDIT known addresses: {}", addresses.len);
            let mut first_address = None;
            for index in 0..addresses.len {
                let address = *addresses.addresses.add(index);
                let bytes: [u8; 32] = std::slice::from_raw_parts(address, 32).try_into().unwrap();
                first_address.get_or_insert(bytes);

                let result = get_balance(node, address, ptr::null());
                let balance = result.value;
                let status = log("get_balance", result);
                eprintln!("AUDIT   balance[{index}] = {balance} ({status:?})");
                log(
                    "get_balance(tip)",
                    get_balance(node, address, &raw const tip),
                );

                let FfiResult {
                    value: notes,
                    error,
                } = get_wallet_notes(node, address, ptr::null());
                log("get_wallet_notes", error);
                for note in 0..notes.len {
                    let _ = (*notes.notes.add(note)).value;
                }
                freed(free_wallet_notes(notes));
            }
            assert!(free_known_addresses(addresses).is_ok());

            assert_ne!(
                log(
                    "get_balance(invalid)",
                    get_balance(node, invalid_key.as_ptr(), ptr::null())
                ),
                OperationStatusCode::Ok
            );
            let FfiResult {
                value: notes,
                error,
            } = get_wallet_notes(node, zero.as_ptr(), ptr::null());
            log("get_wallet_notes(unknown)", error);
            freed(free_wallet_notes(notes));
            let FfiResult {
                value: notes,
                error,
            } = get_wallet_notes(node, invalid_key.as_ptr(), ptr::null());
            assert_ne!(
                log("get_wallet_notes(invalid)", error),
                OperationStatusCode::Ok
            );
            freed(free_wallet_notes(notes));

            for tip_pointer in [ptr::null(), &raw const tip, &raw const zero] {
                let FfiResult { value, error } = get_leader_aged_notes(node, tip_pointer);
                log("get_leader_aged_notes", error);
                for note in 0..value.len {
                    let _ = (*value.notes.add(note)).value;
                }
                freed(free_leader_aged_notes(value));

                let FfiResult { value, error } = get_claimable_vouchers(node, tip_pointer);
                let status = log("get_claimable_vouchers", error);
                if status == OperationStatusCode::Ok {
                    assert!(free_claimable_vouchers(value).is_ok());
                }
            }

            // ---- transactions: argument validation, then a real attempt ----
            let key = first_address.unwrap_or(zero);
            let funding = [key.as_ptr()];
            let null_funding = [ptr::null::<u8>()];
            let invalid_funding = [invalid_key.as_ptr()];

            let transfer = |change: *const u8,
                            funding: *const *const u8,
                            len: usize,
                            recipient: *const u8,
                            amount: u64| {
                let arguments = TransferFundsArguments {
                    optional_tip: ptr::null(),
                    change_public_key: change,
                    funding_public_keys: funding,
                    funding_public_keys_len: len,
                    recipient_public_key: recipient,
                    amount,
                };
                log("transfer_funds", transfer_funds(node, &raw const arguments))
            };
            let np = OperationStatusCode::NullPointer;
            assert_eq!(
                transfer(ptr::null(), funding.as_ptr(), 1, key.as_ptr(), 1),
                np
            );
            assert_eq!(transfer(key.as_ptr(), ptr::null(), 1, key.as_ptr(), 1), np);
            assert_eq!(
                transfer(key.as_ptr(), null_funding.as_ptr(), 1, key.as_ptr(), 1),
                np
            );
            assert_eq!(
                transfer(key.as_ptr(), funding.as_ptr(), 1, ptr::null(), 1),
                np
            );
            assert_ne!(
                transfer(invalid_key.as_ptr(), funding.as_ptr(), 1, key.as_ptr(), 1),
                OperationStatusCode::Ok
            );
            assert_ne!(
                transfer(key.as_ptr(), invalid_funding.as_ptr(), 1, key.as_ptr(), 1),
                OperationStatusCode::Ok
            );
            transfer(key.as_ptr(), funding.as_ptr(), 1, key.as_ptr(), 1);
            transfer(key.as_ptr(), funding.as_ptr(), 1, key.as_ptr(), u64::MAX);

            let metadata = [1u8, 2, 3];
            let deposit = |channel: *const u8, funding_key: *const u8, amount: u64, len: usize| {
                let arguments = ChannelDepositArguments {
                    optional_tip: ptr::null(),
                    channel_id: channel,
                    funding_public_key: funding_key,
                    amount,
                    metadata: if len == usize::MAX {
                        ptr::null()
                    } else {
                        metadata.as_ptr()
                    },
                    metadata_len: if len == usize::MAX { 3 } else { len },
                };
                log(
                    "channel_deposit",
                    channel_deposit(node, &raw const arguments),
                )
            };
            assert_eq!(deposit(ptr::null(), key.as_ptr(), 1, 0), np);
            assert_eq!(deposit(zero.as_ptr(), ptr::null(), 1, 0), np);
            assert_eq!(deposit(zero.as_ptr(), key.as_ptr(), 1, usize::MAX), np);
            assert_ne!(
                deposit(zero.as_ptr(), key.as_ptr(), 0, 0),
                OperationStatusCode::Ok
            );
            assert_ne!(
                deposit(zero.as_ptr(), invalid_key.as_ptr(), 1, 3),
                OperationStatusCode::Ok
            );
            deposit(zero.as_ptr(), key.as_ptr(), 1, 3);
            deposit(zero.as_ptr(), key.as_ptr(), u64::MAX, 3);

            let note_ids = [zero, invalid_key];
            let deposit_with_notes = |notes: *const [u8; 32], notes_len: usize| {
                let arguments = ChannelDepositWithNotesArguments {
                    optional_tip: &raw const tip,
                    channel_id: zero.as_ptr(),
                    input_note_ids: notes,
                    input_note_ids_len: notes_len,
                    metadata: ptr::null(),
                    metadata_len: 0,
                    change_public_key: key.as_ptr(),
                    funding_public_keys: funding.as_ptr(),
                    funding_public_keys_len: 1,
                    max_tx_fee: 0,
                };
                log(
                    "channel_deposit_with_notes",
                    channel_deposit_with_notes(node, &raw const arguments),
                )
            };
            assert_eq!(deposit_with_notes(ptr::null(), 1), np);
            assert_ne!(
                deposit_with_notes(note_ids.as_ptr(), 0),
                OperationStatusCode::Ok
            );
            assert_ne!(
                deposit_with_notes(note_ids.as_ptr(), 2),
                OperationStatusCode::Ok
            );
            deposit_with_notes(note_ids.as_ptr(), 1);

            let not_utf8 = CString::new(vec![0xFF, 0xFE]).unwrap();
            for json in [c"{", c"{}", c"null", not_utf8.as_c_str()] {
                assert_ne!(
                    string_result("wallet_fund_tx", wallet_fund_tx(node, json.as_ptr())),
                    OperationStatusCode::Ok
                );
                assert_ne!(
                    log(
                        "submit_signed_transaction",
                        submit_signed_transaction(node, json.as_ptr())
                    ),
                    OperationStatusCode::Ok
                );
            }

            // ---- leader / blend / pow ----
            log("leader_claim", leader_claim(node));
            for locator in [
                c"not a locator",
                c"/ip4/127.0.0.1/udp/3400/quic-v1",
                not_utf8.as_c_str(),
            ] {
                log(
                    "blend_join_as_core_node",
                    blend_join_as_core_node(node, locator.as_ptr(), zero.as_ptr()),
                );
            }
            assert_ne!(
                log(
                    "blend_join_as_core_node(invalid note)",
                    blend_join_as_core_node(
                        node,
                        c"/ip4/127.0.0.1/udp/3400/quic-v1".as_ptr(),
                        invalid_key.as_ptr()
                    )
                ),
                OperationStatusCode::Ok
            );

            let FfiResult { value, error } = pow_status(node);
            log("pow_status", error);
            for target in 0..value.auto_claim.targets_len {
                let _ = (*value.auto_claim.targets.add(target)).threshold;
            }
            freed(free_pow_status(value));

            let FfiResult { value, error } = pow_claimable_rewards(node);
            if log("pow_claimable_rewards", error) == OperationStatusCode::Ok {
                for slot in 0..value.len {
                    let _ = *value.slots_until_expiry.add(slot);
                }
                assert!(free_pow_claimable_rewards(value).is_ok());
            }
            log("pow_claim(null)", pow_claim(node, ptr::null()));
            log("pow_claim(key)", pow_claim(node, key.as_ptr()));
            assert_ne!(
                log("pow_claim(invalid)", pow_claim(node, invalid_key.as_ptr())),
                OperationStatusCode::Ok
            );
            log("pow_start_auto_claim", pow_start_auto_claim(node));
            log("pow_stop_auto_claim", pow_stop_auto_claim(node));
            log("pow_start_mining", pow_start_mining(node));
            log("pow_stop_mining", pow_stop_mining(node));

            // ---- concurrent callers on one handle ----
            let shared = AtomicPtr::new(node);
            std::thread::scope(|scope| {
                for _ in 0..4 {
                    scope.spawn(|| {
                        let node = shared.load(Ordering::SeqCst);
                        for _ in 0..10 {
                            let result = get_cryptarchia_info(node);
                            assert!(result.is_ok());
                            assert!(free_cryptarchia_info(result.value).is_ok());
                            assert_eq!(
                                string_result("get_chain_id", get_chain_id(node)),
                                OperationStatusCode::Ok
                            );
                        }
                    });
                }
            });

            // ---- let the subscriptions see some blocks ----
            let deadline = Instant::now() + Duration::from_secs(wait_secs());
            while Instant::now() < deadline && PROCESSED.snapshot().0 < 2 {
                std::thread::sleep(Duration::from_millis(250));
            }
            eprintln!(
                "AUDIT before shutdown (events, sentinels): new_blocks={:?} processed={:?} lib={:?}",
                NEW_BLOCKS.snapshot(),
                PROCESSED.snapshot(),
                LIB.snapshot()
            );

            let (status, message) = consume(shutdown_node(node));
            assert_eq!(status, OperationStatusCode::Ok, "{message}");
        }

        let at_shutdown = (NEW_BLOCKS.snapshot(), PROCESSED.snapshot(), LIB.snapshot());
        std::thread::sleep(Duration::from_secs(1));
        let later = (NEW_BLOCKS.snapshot(), PROCESSED.snapshot(), LIB.snapshot());
        eprintln!(
            "AUDIT after shutdown (events, sentinels): new_blocks={:?} processed={:?} lib={:?}",
            later.0, later.1, later.2
        );
        assert_eq!(
            at_shutdown, later,
            "A callback ran after shutdown_node returned"
        );
        for (name, (_, sentinels)) in [
            ("new_blocks", later.0),
            ("processed", later.1),
            ("lib", later.2),
        ] {
            assert!(sentinels <= 1, "{name}: more than one end-of-stream call");
        }
        drop(paths);
    }

    /// Starting a node must leave the host's panic hook in place: the node
    /// binary's own hook exits the process, which would end this test.
    #[test]
    #[serial]
    fn start_keeps_the_host_panic_hook() {
        static HOST_HOOK_CALLS: AtomicUsize = AtomicUsize::new(0);

        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {
            HOST_HOOK_CALLS.fetch_add(1, Ordering::SeqCst);
        }));

        let paths = TestConfigPaths::new();
        let node = paths.start();
        let while_running = std::thread::spawn(|| panic!("host panic while the node runs")).join();
        std::thread::sleep(Duration::from_secs(2));
        let shutdown = consume(unsafe { shutdown_node(node) });
        let after_shutdown = std::thread::spawn(|| panic!("host panic after shutdown")).join();

        std::panic::set_hook(previous);

        assert!(while_running.is_err());
        assert!(after_shutdown.is_err());
        assert_eq!(HOST_HOOK_CALLS.load(Ordering::SeqCst), 2);
        assert_eq!(shutdown.0, OperationStatusCode::Ok, "{}", shutdown.1);
    }

    /// Start/stop cycles must not accumulate memory or leave the state
    /// directory locked.
    #[test]
    #[serial]
    fn restart_cycles() {
        let paths = TestConfigPaths::new();
        for _ in 0..2 {
            let node = paths.start();
            std::thread::sleep(Duration::from_secs(2));
            let result = unsafe { get_time_info(node) };
            assert!(result.is_ok());
            assert!(free_time_info(result.value).is_ok());
            let (status, message) = consume(unsafe { shutdown_node(node) });
            assert_eq!(status, OperationStatusCode::Ok, "{message}");
        }
        let _ = paths.temp_dir.path();
    }
}

/// Each of these takes the process down. Run one at a time:
///
/// ```text
/// cargo test -p logos-blockchain-c -- --ignored --exact ffi_safety_tests::crashers::<name>
/// ```
mod crashers {
    use super::*;

    static NODE: AtomicPtr<LogosBlockchainNode> = AtomicPtr::new(ptr::null_mut());
    static CALLS: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn reentrant(data: *const c_char) {
        if data.is_null() {
            return;
        }
        CALLS.fetch_add(1, Ordering::SeqCst);
        let result = unsafe { get_time_info(NODE.load(Ordering::SeqCst)) };
        eprintln!("AUDIT survived re-entrant call: ok={}", result.is_ok());
    }

    /// Callbacks run on a runtime worker; every node API uses `block_on`,
    /// which panics there.
    #[test]
    #[ignore = "aborts: calling the API from a subscription callback"]
    fn api_call_from_callback() {
        let paths = TestConfigPaths::new();
        let node = paths.start();
        NODE.store(node, Ordering::SeqCst);
        assert!(unsafe { subscribe_to_processed_blocks(node, reentrant) }.is_ok());
        let deadline = Instant::now() + Duration::from_secs(180);
        while Instant::now() < deadline && CALLS.load(Ordering::SeqCst) == 0 {
            std::thread::sleep(Duration::from_millis(250));
        }
        eprintln!("AUDIT callback calls: {}", CALLS.load(Ordering::SeqCst));
        let _status = consume(unsafe { shutdown_node(node) });
    }
}
