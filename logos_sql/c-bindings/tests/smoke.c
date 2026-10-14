#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "logos_sql.h"

static void check_ok(SqlStatus status) {
    if (status.code != SqlOk) {
        fprintf(stderr, "Logos SQL error %d: %s\n", status.code, status.message);
        logos_sql_free_status(status);
        exit(1);
    }
    logos_sql_free_status(status);
}

int main(int argc, char **argv) {
    SqlDatabaseResult invalid = logos_sql_open(NULL, 100);
    assert(invalid.value == NULL);
    assert(invalid.error.code == SqlNullPointer);
    logos_sql_free_status(invalid.error);

    invalid = logos_sql_open("{}", 100);
    assert(invalid.value == NULL);
    assert(invalid.error.code == SqlInvalidArgument);
    logos_sql_free_status(invalid.error);

    logos_sql_free_string(NULL);

    if (argc == 1) {
        puts("C ABI error and cleanup checks passed");
        return 0;
    }

    assert(argc == 2);
    FILE *config_file = fopen(argv[1], "rb");
    assert(config_file != NULL);
    char config[16384];
    size_t count = fread(config, 1, sizeof(config) - 1, config_file);
    assert(!ferror(config_file));
    assert(feof(config_file));
    fclose(config_file);
    config[count] = '\0';

    SqlDatabaseResult opened = logos_sql_open(config, 30000);
    check_ok(opened.error);
    assert(opened.value != NULL);

    for (unsigned view = SqlLatest; view <= SqlFinalized; ++view) {
        SqlStringResult rows = logos_sql_query(
            opened.value, view, "SELECT ?1 AS value",
            "[{\"type\":\"integer\",\"value\":\"9223372036854775807\"}]", 10, 1024);
        check_ok(rows.error);
        assert(strstr(rows.value, "9223372036854775807") != NULL);
        logos_sql_free_string(rows.value);
    }

    check_ok(logos_sql_close(opened.value));
    puts("C ABI replica lifecycle and both read views passed");
    return 0;
}
