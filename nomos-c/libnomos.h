#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>

enum NomosNodeErrorCode {
  None = 0,
  CouldNotInitialize = 1,
  StopError = 2,
  NullPtr = 3,
};
typedef uint8_t NomosNodeErrorCode;

typedef struct NomosNode {
  void *overwatch;
  void *runtime;
} NomosNode;

typedef struct InitializedNomosNodeResult {
  struct NomosNode *nomos_node;
  NomosNodeErrorCode error_code;
} InitializedNomosNodeResult;

struct InitializedNomosNodeResult start_nomos_node(const char *config_path);

/**
 * # Safety
 *
 * The caller must ensure that:
 * - `node` is a valid pointer to a `NomosNode` instance
 * - The `NomosNode` instance was created by this library
 * - The pointer will not be used after this function returns
 */
NomosNodeErrorCode stop_node(struct NomosNode *node);
