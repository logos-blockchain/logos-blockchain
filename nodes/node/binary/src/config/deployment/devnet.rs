pub const NAME: &str = "devnet";

pub const SERIALIZED_DEPLOYMENT: &str = "
blend:
  common:
    num_blend_layers: 1
    minimum_network_size: 2
    protocol_name: /logos-blockchain-devnet/blend/1.0.0
    data_replication_factor: 0
  core:
    scheduler:
      cover:
        message_frequency_per_round: 1.0
        intervals_for_safety_buffer: 100
      delayer:
        maximum_release_delay_in_rounds: 1
    minimum_messages_coefficient: 1
    normalization_constant: 1.03
    activity_threshold_sensitivity: 1
network:
  kademlia_protocol_name: /logos-blockchain-devnet/kad/1.0.0
  identify_protocol_name: /logos-blockchain-devnet/identify/1.0.0
  chain_sync_protocol_name: /logos-blockchain-devnet/chainsync/1.0.0
cryptarchia:
  epoch_config:
    epoch_stake_distribution_stabilization: 3
    epoch_period_nonce_buffer: 3
    epoch_period_nonce_stabilization: 4
  security_param: 30
  slot_activation_coeff:
    numerator: 1
    denominator: 20
  learning_rate: 0.5
  sdp_config:
    service_params:
      BN:
        lock_period: 10
        inactivity_period: 1
        retention_period: 1
        timestamp: 0
    min_stake:
      threshold: 1
      timestamp: 0
  gossipsub_protocol: /logos-blockchain-devnet/cryptarchia/1.0.0
  genesis_block:
    header:
      version: Bedrock
      parent_block: '0000000000000000000000000000000000000000000000000000000000000000'
      slot: 0
      block_root: b5f8787ac23674822414c70eea15d842da38f2e806ede1a73cf7b5cf0277da07
      proof_of_leadership:
        proof: '0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'
        entropy_contribution: '0000000000000000000000000000000000000000000000000000000000000000'
        leader_key: '0000000000000000000000000000000000000000000000000000000000000000'
        voucher_cm: '0000000000000000000000000000000000000000000000000000000000000000'
    signature: '00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'
    transactions:
    - mantle_tx:
        ops:
        - opcode: 0
          payload:
            inputs: []
            outputs:
            - value: 1
              pk: d204000000000000000000000000000000000000000000000000000000000000
            - value: 100
              pk: '2e03b2eff5a45478e7e79668d2a146cf2c5c7925bce927f2b1c67f2ab4fc0d26'
            - value: 1
              pk: ed266e6e887b9b97059dc1aa1b7b2e19b934291753c6336a163fe4ebaa28e717
        - opcode: 17
          payload:
            channel_id: '0000000000000000000000000000000000000000000000000000000000000000'
            # chain_id_len=12 (u64_le), chain_id=logos-devnet (utf-8),
            # genesis_time=2026-01-10T07:47:56Z (u64_le), epoch_nonce=[0u8; 32]
            inscription: '0c000000000000006c6f676f732d6465766e65742c046269000000000000000000000000000000000000000000000000000000000000000000000000'
            parent: '0000000000000000000000000000000000000000000000000000000000000000'
            signer: '0000000000000000000000000000000000000000000000000000000000000000'
        - opcode: 32
          payload:
            service_type: BN
            locators: []
            provider_id: '86c8519f00178e9eb1fe5f4247e4bed77d4c9f6da2fb10e8a1fdd7ba6bc79fa0'
            zk_id: '64249c75c2cb813578b75d05b215fc95f67cea5862fff047228183f22e63460e'
            locked_note_id: '0000000000000000000000000000000000000000000000000000000000000000'
        execution_gas_price: 0
        storage_gas_price: 0
      ops_proofs:
      - !Ed25519Sig '00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'
      - !Ed25519Sig '00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'
      - !Ed25519Sig '00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'
  faucet_pk: faf8a7e44e9f45d35efcd9043c4a55095339e229c4115dbeb6231e2b8422f610
time:
  slot_duration: '1.000000000'
mempool:
  pubsub_topic: /logos-blockchain-devnet/mempool/1.0.0

";
