#USE_LOCAL_HOST_NTP_TIME_CONFIG=true \

LOGOS_BLOCKCHAIN_NODE_BIN=/Users/yjlee/repos/logos-blockchain/target/release/logos-blockchain-node \
cargo test --locked -p logos-blockchain-tests --features cucumber --test cucumber -- --name "IBD scales across discovered peers under trusted-peer bottleneck"
