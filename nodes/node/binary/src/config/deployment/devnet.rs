pub const NAME: &str = "devnet";

pub const SERIALIZED_DEPLOYMENT: &str = "
blend:
  common:
    num_blend_layers: 1
    minimum_network_size: 32
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
      block_root: d196415bb6ad4780d89c83dd9714e1666004ee572a9c9829ba57a0aca350f008
      proof_of_leadership:
        proof:
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        - 0
        entropy_contribution: '0000000000000000000000000000000000000000000000000000000000000000'
        leader_key: '0000000000000000000000000000000000000000000000000000000000000000'
        voucher_cm: '0000000000000000000000000000000000000000000000000000000000000000'
    signature:
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    - 0
    transactions:
    - mantle_tx:
        ops:
        - opcode: 0
          payload:
            inputs: []
            outputs:
            - value: 100000
              pk: eb3158fd1e5b25449eec9dc753595da361295e85ad93bb82b01f6db0c70a600a
            - value: 1
              pk: '3c147e1d8a44e726903ff4d3c82d08a8c6e07b34ae6ae2fe19f28af005693a0d'
            - value: 100
              pk: '0ead813832eea418b36d09724d19db5a07f86142b676a53cfcdc968b66f14002'
            - value: 100000
              pk: cd035cf2ca2b16a52cada554796335f1a69c132aa9997967aba878e72cd6f507
            - value: 1
              pk: f406445a75c65716774d133f878e288e097e34e76a9f2b49dca0107ad63b9c17
            - value: 100
              pk: '422dfbd9835e4a9f99e5eeb20b75d302db3487f9435cd6b4089d31311dbf4b0e'
            - value: 100000
              pk: '9c5043de2ccf00a5deb078598788646387b6c98260e4112495b4e2d101c89a2e'
            - value: 1
              pk: '4b71288e57901f2f33fa7c3609c311447f401f08f0bec9c1076d5cdbe5d47909'
            - value: 100
              pk: '45085fb7118152739a6d95b4c27b004bfc39b58fa6fc7dfe564e04d1dde69422'
            - value: 100000
              pk: a4aafde553c92aa1947da3b31188877f3f11362dbd5537cff5a02dd63982c70c
            - value: 1
              pk: '23c9dc6813bf5d090f8f5119c48a86ff808bbc0371cfef67af3551e63bcba20c'
            - value: 100
              pk: be8492b9c322485c707ff92fe0785163feb63273c82417f14b1ef6b90674e90e
            - value: 18446744073709151211
              pk: faf8a7e44e9f45d35efcd9043c4a55095339e229c4115dbeb6231e2b8422f610
        - opcode: 17
          payload:
            channel_id: '0000000000000000000000000000000000000000000000000000000000000000'
            inscription:
            - 103
            - 101
            - 110
            - 101
            - 115
            - 105
            - 115
            parent: '0000000000000000000000000000000000000000000000000000000000000000'
            signer: '0000000000000000000000000000000000000000000000000000000000000000'
        - opcode: 32
          payload:
            service_type: BN
            locators:
            - /ip4/65.109.51.37/udp/3400/quic-v1
            provider_id: '98fd43b126e47b844733edf5c00527968c44e6d957ea4d024b27b8bc939d1cc1'
            zk_id: '55d8bdbfeccd06f73da5a7af16e5658b634e55bc5829fbe980310f4c99545e2d'
            locked_note_id: d5c3939a9ba07d0ab4a9c279eff2abfda35a19ebdbc16688bdfb32ac6a149d0c
        - opcode: 32
          payload:
            service_type: BN
            locators:
            - /ip4/65.109.51.37/udp/3401/quic-v1
            provider_id: c8e9716ab4573beed1bb5c0a1b5142422059bba7662488542d5360b6f1f1faa2
            zk_id: c1c5e2411f2f41b711b6833c254eda79742eb28b4016c0af29f517f02610a109
            locked_note_id: '07a07a71a8168b98e849a83bf13e426a8bcf4f15065bab8c2f0baece48f8d500'
        - opcode: 32
          payload:
            service_type: BN
            locators:
            - /ip4/65.109.51.37/udp/3402/quic-v1
            provider_id: '8c213c337f5fc2046c25a6e811fa14267e82891cb1931a2b6e0d3f0e9caa93c0'
            zk_id: '9f50f7a640d6bc36db15ed4397eaf04792f844e089c4bc4052e59d38817ff31b'
            locked_note_id: e3581158cc4174328ad04b06fe453dc3d2601a39f6a0a0cbb621e38edfe23907
        - opcode: 32
          payload:
            service_type: BN
            locators:
            - /ip4/65.109.51.37/udp/3403/quic-v1
            provider_id: '0e78cd0bf8cff5faa364776ac024c4c632c48f8a9d35789541a1b71009c3537e'
            zk_id: ea7c784a570634bb4afb67553336984a9ce40eec02ed3d9249bd6d093b281f30
            locked_note_id: f31739eb4397e7e117a6422d84eb89388765c86d4d2a912625a6d821a4bbe701
        execution_gas_price: 0
        storage_gas_price: 0
      ops_proofs:
      - NoProof
      - NoProof
      - NoProof
      - NoProof
      - NoProof
      - NoProof
  faucet_pk: faf8a7e44e9f45d35efcd9043c4a55095339e229c4115dbeb6231e2b8422f610
time:
  slot_duration: '1.000000000'
  chain_start_time: 2026-03-03 14:49:11.0 +00:00:00
mempool:
  pubsub_topic: /logos-blockchain-devnet/mempool/1.0.0
";
