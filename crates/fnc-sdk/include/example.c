/* Smoke test of the C header (compile + link only; no instance needs to run). */
#include <stdio.h>
#include "fennec_sdk.h"

int main(void)
{
    static const FNC_WCHAR search[] = {'a', 'b', 'c', 0};
    fnc_SetSearchW(search);
    fnc_SetRequestFlags(FNC_REQUEST_FILE_NAME | FNC_REQUEST_PATH | FNC_REQUEST_SIZE);
    fnc_SetSort(FNC_SORT_SIZE_DESCENDING);
    fnc_SetMax(10);
    if (!fnc_QueryW(1)) {
        printf("query failed: %u\n", (unsigned)fnc_GetLastError());
        return fnc_GetLastError() == FNC_ERROR_IPC ? 0 : 1;
    }
    for (FNC_DWORD i = 0; i < fnc_GetNumResults(); i++) {
        FNC_SIZE size;
        fnc_GetResultSize(i, &size);
        printf("%s %llu\n", fnc_GetResultFileNameA(i), (unsigned long long)size);
    }
    fnc_CleanUp();
    return 0;
}
