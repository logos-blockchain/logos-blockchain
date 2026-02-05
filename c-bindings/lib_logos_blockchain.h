#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>

typedef enum State {
  Bootstrapping = 0,
  Online = 1,
} State;

typedef enum OperationStatus {
  Ok = 0,
  NotFound = 1,
  NullPointer = 2,
  RelayError = 3,
  ChannelSendError = 4,
  ChannelReceiveError = 5,
  ServiceError = 6,
  RuntimeError = 7,
  DynError = 8,
  InitializationError = 9,
  StopError = 10,
} OperationStatus;

typedef uint8_t Hash[32];

typedef Hash HeaderId;

typedef struct CryptarchiaInfo {
  HeaderId lib;
  HeaderId tip;
  uint64_t slot;
  uint64_t height;
  enum State mode;
} CryptarchiaInfo;

/**
 * Simple wrapper around a pointer to a value or an error.
 *
 * Pointer is not guaranteed. You should check the error field before
 * dereferencing the pointer.
 */
typedef struct PointerResult_CryptarchiaInfo__OperationStatus {
  struct CryptarchiaInfo *value;
  enum OperationStatus error;
} PointerResult_CryptarchiaInfo__OperationStatus;

typedef struct PointerResult_CryptarchiaInfo__OperationStatus CryptarchiaInfoResult;

typedef struct LogosBlockchainNode {
  void *overwatch;
  void *runtime;
} LogosBlockchainNode;

/**
 * Simple wrapper around a pointer to a value or an error.
 *
 * Pointer is not guaranteed. You should check the error field before
 * dereferencing the pointer.
 */
typedef struct PointerResult_LogosBlockchainNode__OperationStatus {
  struct LogosBlockchainNode *value;
  enum OperationStatus error;
} PointerResult_LogosBlockchainNode__OperationStatus;

typedef struct PointerResult_LogosBlockchainNode__OperationStatus InitializedLogosBlockchainNodeResult;

typedef void (*CCallback______c_char)(const char *data);

typedef uint64_t Value;

/**
 * Simple wrapper around a value or an error.
 *
 * Value is not guaranteed. You should check the error field before accessing
 * the value.
 */
typedef struct ValueResult_Value__OperationStatus {
  Value value;
  enum OperationStatus error;
} ValueResult_Value__OperationStatus;

typedef struct ValueResult_Value__OperationStatus BalanceResult;

/**
 * Simple wrapper around a value or an error.
 *
 * Value is not guaranteed. You should check the error field before accessing
 * the value.
 */
typedef struct ValueResult_Hash__OperationStatus {
  Hash value;
  enum OperationStatus error;
} ValueResult_Hash__OperationStatus;

typedef struct ValueResult_Hash__OperationStatus TransferFundsResult;

typedef struct TransferFundsArguments {
  const HeaderId *optional_tip;
  const uint8_t *change_public_key;
  const uint8_t *const *funding_public_keys;
  uintptr_t funding_public_keys_len;
  const uint8_t *recipient_public_key;
  uint64_t amount;
} TransferFundsArguments;

/**
 * Get the current Cryptarchia info.
 *
 * # Arguments
 *
 * - `node`: A non-null pointer to a [`LogosBlockchainNode`].
 *
 * # Returns
 *
 * A [`CryptarchiaInfoResult`] containing a pointer to the allocated
 * [`CryptarchiaInfo`] struct on success, or an [`OperationStatus`] error on
 * failure.
 *
 * # Safety
 *
 * This function is unsafe because it dereferences raw pointers.
 * The caller must ensure that all pointers are non-null and point to valid
 * memory.
 *
 * # Memory Management
 *
 * This function allocates memory for the output [`CryptarchiaInfo`] struct.
 * The caller must free this memory using the [`free_cryptarchia_info`]
 * function.
 */
CryptarchiaInfoResult get_cryptarchia_info(const struct LogosBlockchainNode *node);

/**
 * Frees the memory allocated for a [`CryptarchiaInfo`] struct.
 *
 * # Arguments
 *
 * - `pointer`: A pointer to the [`CryptarchiaInfo`] struct to be freed.
 */
void free_cryptarchia_info(struct CryptarchiaInfo *pointer);

/**
 * Creates and starts a Logos blockchain node based on the provided
 * configuration file path.
 *
 * # Arguments
 *
 * - `config_path`: A pointer to a string representing the path to the
 *   configuration file.
 * - `deployment`: A pointer to a string representing either a well-known
 *   deployment name (e.g., "devnet") or a path to a deployment YAML file. If
 *   null, defaults to "devnet".
 *
 * # Returns
 *
 * An `InitializedLogosBlockchainNodeResult` containing either a pointer to the
 * initialized `LogosBlockchainNode` or an error code.
 */
InitializedLogosBlockchainNodeResult start_lb_node(const char *config_path, const char *deployment);

/**
 * Stops and frees the resources associated with the given Logos blockchain
 * node.
 *
 * # Arguments
 *
 * - `node`: A pointer to the `LogosBlockchainNode` instance to be stopped.
 *
 * # Returns
 *
 * An `OperationStatus` indicating success or failure.
 *
 * # Safety
 *
 * The caller must ensure that:
 * - `node` is a valid pointer to a `LogosBlockchainNode` instance
 * - The `LogosBlockchainNode` instance was created by this library
 * - The pointer will not be used after this function returns
 */
enum OperationStatus stop_node(struct LogosBlockchainNode *node);

void subscribe_to_new_blocks(const struct LogosBlockchainNode *node,
                             CCallback______c_char callback_per_block);

/**
 * Get the balance of a wallet address
 *
 * # Arguments
 *
 * - `node`: A non-null pointer to a [`LogosBlockchainNode`] instance.
 * - `wallet_address`: A non-null pointer to the public key bytes of the wallet
 *   address to query.
 * - `optional_tip`: An optional pointer to the header ID to query the balance
 *   at. If null, the current tip will be used.
 *
 * # Returns
 *
 * A [`ValueResult`] containing the balance on success, or an
 * [`OperationStatus`] error on failure.
 *
 * # Safety
 *
 * This function is unsafe because it dereferences raw pointers. The caller
 * must ensure that all pointers are valid.
 */
BalanceResult get_balance(const struct LogosBlockchainNode *node,
                          const uint8_t *wallet_address,
                          const HeaderId *optional_tip);

/**
 * Transfer funds from some addresses to another.
 *
 * # Arguments
 *
 * - `node`: A non-null pointer to a [`LogosBlockchainNode`] instance.
 * - `arguments`: A non-null pointer to a [`TransferFundsArguments`] struct
 *   containing the transaction arguments.
 *
 * # Returns
 *
 * A [`TransferFundsResult`] containing a pointer to a [`Hash`] where the
 * transaction hash will be written on success, or an [`OperationStatus`] error
 * on failure. The hash will be written in little-endian format.
 *
 * # Safety
 *
 * This function is unsafe because it dereferences raw pointers. The caller
 * must ensure that all pointers are valid.
 *
 * # Memory Management
 *
 * This function allocates memory for the output [`CryptarchiaInfo`] struct.
 * The caller must free this memory using the [`free_cryptarchia_info`]
 * function.
 */
TransferFundsResult transfer_funds(const struct LogosBlockchainNode *node,
                                   const struct TransferFundsArguments *arguments);

/**
 * Frees the memory allocated for a [`Hash`] value.
 *
 * # Arguments
 *
 * - `pointer`: A pointer to the [`Hash`] to be freed.
 */
void free_transfer_funds(Hash *pointer);

bool is_ok(const enum OperationStatus *self);
