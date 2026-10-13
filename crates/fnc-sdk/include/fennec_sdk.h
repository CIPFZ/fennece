/*
 * fennec_sdk.h - C interface of the Fennec SDK (fennec_sdk.dll / libfennec_sdk.so).
 *
 * Mirrors the Everything SDK (Everything.h): replace the `Everything_` prefix with `fnc_`
 * and the EVERYTHING_ constants with FNC_. Talks to a running Fennec over its native IPC
 * pipe (\\.\pipe\Fennec.<instance>.ipc).
 *
 * Differences from the Everything SDK:
 *   - fnc_QueryW/fnc_QueryA are always synchronous; bWait, reply window and reply id are
 *     ignored.
 *   - "A" functions take and return UTF-8.
 *   - fnc_GetTotFileResults/fnc_GetTotFolderResults are not provided; the per-page
 *     counts fnc_GetNumFileResults/fnc_GetNumFolderResults are.
 *   - fnc_SetInstanceW selects a named instance (Everything uses -instance on the client).
 *
 * Strings returned by fnc_GetResult* stay valid until the next fnc_Query*, fnc_Reset or
 * fnc_CleanUp. All functions are thread-safe (one global state, like the Everything SDK).
 */

#ifndef FENNEC_SDK_H
#define FENNEC_SDK_H

#include <stdint.h>

#ifdef _WIN32
#include <wchar.h>
typedef wchar_t FNC_WCHAR; /* UTF-16 */
#define FNC_API __declspec(dllimport)
#else
typedef uint16_t FNC_WCHAR; /* UTF-16 code unit (not wchar_t, which is 32-bit here) */
#define FNC_API
#endif

typedef int32_t FNC_BOOL;
typedef uint32_t FNC_DWORD;
typedef uint64_t FNC_FILETIME; /* 100 ns ticks since 1601-01-01 UTC; 0xFFFFFFFFFFFFFFFF = unknown */
typedef uint64_t FNC_SIZE;     /* bytes; 0xFFFFFFFFFFFFFFFF = folder / unknown */

#ifdef __cplusplus
extern "C" {
#endif

/* fnc_GetLastError() */
#define FNC_OK 0
#define FNC_ERROR_MEMORY 1
#define FNC_ERROR_IPC 2
#define FNC_ERROR_REGISTERCLASSEX 3
#define FNC_ERROR_CREATEWINDOW 4
#define FNC_ERROR_CREATETHREAD 5
#define FNC_ERROR_INVALIDINDEX 6
#define FNC_ERROR_INVALIDCALL 7
#define FNC_ERROR_INVALIDREQUEST 8
#define FNC_ERROR_INVALIDPARAMETER 9

/* fnc_SetSort() / fnc_GetResultListSort() (same values as EVERYTHING_SORT_*) */
#define FNC_SORT_NAME_ASCENDING 1
#define FNC_SORT_NAME_DESCENDING 2
#define FNC_SORT_PATH_ASCENDING 3
#define FNC_SORT_PATH_DESCENDING 4
#define FNC_SORT_SIZE_ASCENDING 5
#define FNC_SORT_SIZE_DESCENDING 6
#define FNC_SORT_EXTENSION_ASCENDING 7
#define FNC_SORT_EXTENSION_DESCENDING 8
#define FNC_SORT_TYPE_NAME_ASCENDING 9
#define FNC_SORT_TYPE_NAME_DESCENDING 10
#define FNC_SORT_DATE_CREATED_ASCENDING 11
#define FNC_SORT_DATE_CREATED_DESCENDING 12
#define FNC_SORT_DATE_MODIFIED_ASCENDING 13
#define FNC_SORT_DATE_MODIFIED_DESCENDING 14
#define FNC_SORT_ATTRIBUTES_ASCENDING 15
#define FNC_SORT_ATTRIBUTES_DESCENDING 16
#define FNC_SORT_FILE_LIST_FILENAME_ASCENDING 17
#define FNC_SORT_FILE_LIST_FILENAME_DESCENDING 18
#define FNC_SORT_RUN_COUNT_ASCENDING 19
#define FNC_SORT_RUN_COUNT_DESCENDING 20
#define FNC_SORT_DATE_RECENTLY_CHANGED_ASCENDING 21
#define FNC_SORT_DATE_RECENTLY_CHANGED_DESCENDING 22
#define FNC_SORT_DATE_ACCESSED_ASCENDING 23
#define FNC_SORT_DATE_ACCESSED_DESCENDING 24
#define FNC_SORT_DATE_RUN_ASCENDING 25
#define FNC_SORT_DATE_RUN_DESCENDING 26

/* fnc_SetRequestFlags() (same bits as EVERYTHING_REQUEST_*) */
#define FNC_REQUEST_FILE_NAME 0x00000001
#define FNC_REQUEST_PATH 0x00000002
#define FNC_REQUEST_FULL_PATH_AND_FILE_NAME 0x00000004
#define FNC_REQUEST_EXTENSION 0x00000008
#define FNC_REQUEST_SIZE 0x00000010
#define FNC_REQUEST_DATE_CREATED 0x00000020
#define FNC_REQUEST_DATE_MODIFIED 0x00000040
#define FNC_REQUEST_DATE_ACCESSED 0x00000080
#define FNC_REQUEST_ATTRIBUTES 0x00000100
#define FNC_REQUEST_FILE_LIST_FILE_NAME 0x00000200
#define FNC_REQUEST_RUN_COUNT 0x00000400
#define FNC_REQUEST_DATE_RUN 0x00000800
#define FNC_REQUEST_DATE_RECENTLY_CHANGED 0x00001000
#define FNC_REQUEST_HIGHLIGHTED_FILE_NAME 0x00002000
#define FNC_REQUEST_HIGHLIGHTED_PATH 0x00004000
#define FNC_REQUEST_HIGHLIGHTED_FULL_PATH_AND_FILE_NAME 0x00008000

/* fnc_GetTargetMachine() */
#define FNC_TARGET_MACHINE_X86 1
#define FNC_TARGET_MACHINE_X64 2
#define FNC_TARGET_MACHINE_ARM 3
#define FNC_TARGET_MACHINE_ARM64 4

/* search state */
FNC_API void fnc_SetSearchW(const FNC_WCHAR *lpString);
FNC_API void fnc_SetSearchA(const char *lpString);
FNC_API const FNC_WCHAR *fnc_GetSearchW(void);
FNC_API const char *fnc_GetSearchA(void);
FNC_API void fnc_SetInstanceW(const FNC_WCHAR *lpInstanceName);
FNC_API void fnc_SetMatchPath(FNC_BOOL bEnable);
FNC_API void fnc_SetMatchCase(FNC_BOOL bEnable);
FNC_API void fnc_SetMatchWholeWord(FNC_BOOL bEnable);
FNC_API void fnc_SetRegex(FNC_BOOL bEnable);
FNC_API void fnc_SetMatchDiacritics(FNC_BOOL bEnable);
FNC_API void fnc_SetMax(FNC_DWORD dwMax);
FNC_API void fnc_SetOffset(FNC_DWORD dwOffset);
FNC_API void fnc_SetSort(FNC_DWORD dwSort);
FNC_API void fnc_SetRequestFlags(FNC_DWORD dwRequestFlags);
FNC_API void fnc_SetReplyWindow(intptr_t hWnd); /* ignored */
FNC_API void fnc_SetReplyID(FNC_DWORD dwId);    /* ignored */
FNC_API FNC_BOOL fnc_GetMatchPath(void);
FNC_API FNC_BOOL fnc_GetMatchCase(void);
FNC_API FNC_BOOL fnc_GetMatchWholeWord(void);
FNC_API FNC_BOOL fnc_GetRegex(void);
FNC_API FNC_DWORD fnc_GetMax(void);
FNC_API FNC_DWORD fnc_GetOffset(void);
FNC_API FNC_DWORD fnc_GetSort(void);
FNC_API FNC_DWORD fnc_GetRequestFlags(void);
FNC_API FNC_DWORD fnc_GetLastError(void);

/* query */
FNC_API FNC_BOOL fnc_QueryW(FNC_BOOL bWait);
FNC_API FNC_BOOL fnc_QueryA(FNC_BOOL bWait);
FNC_API void fnc_SortResultsByPath(void);
FNC_API FNC_DWORD fnc_GetResultListSort(void);
FNC_API FNC_DWORD fnc_GetResultListRequestFlags(void);

/* results */
FNC_API FNC_DWORD fnc_GetNumResults(void);
FNC_API FNC_DWORD fnc_GetNumFileResults(void);
FNC_API FNC_DWORD fnc_GetNumFolderResults(void);
FNC_API FNC_DWORD fnc_GetTotResults(void);
FNC_API FNC_BOOL fnc_IsVolumeResult(FNC_DWORD dwIndex);
FNC_API FNC_BOOL fnc_IsFolderResult(FNC_DWORD dwIndex);
FNC_API FNC_BOOL fnc_IsFileResult(FNC_DWORD dwIndex);
FNC_API const FNC_WCHAR *fnc_GetResultFileNameW(FNC_DWORD dwIndex);
FNC_API const char *fnc_GetResultFileNameA(FNC_DWORD dwIndex);
FNC_API const FNC_WCHAR *fnc_GetResultPathW(FNC_DWORD dwIndex);
FNC_API const char *fnc_GetResultPathA(FNC_DWORD dwIndex);
/* Copies at most dwBufSize-1 units plus a terminator; returns the units copied. With
   buf == NULL returns the length needed (without the terminator). */
FNC_API FNC_DWORD fnc_GetResultFullPathNameW(FNC_DWORD dwIndex, FNC_WCHAR *buf, FNC_DWORD dwBufSize);
FNC_API FNC_DWORD fnc_GetResultFullPathNameA(FNC_DWORD dwIndex, char *buf, FNC_DWORD dwBufSize);
FNC_API const FNC_WCHAR *fnc_GetResultExtensionW(FNC_DWORD dwIndex);
FNC_API const char *fnc_GetResultExtensionA(FNC_DWORD dwIndex);
FNC_API FNC_BOOL fnc_GetResultSize(FNC_DWORD dwIndex, FNC_SIZE *lpSize);
FNC_API FNC_BOOL fnc_GetResultDateCreated(FNC_DWORD dwIndex, FNC_FILETIME *lpDateCreated);
FNC_API FNC_BOOL fnc_GetResultDateModified(FNC_DWORD dwIndex, FNC_FILETIME *lpDateModified);
FNC_API FNC_BOOL fnc_GetResultDateAccessed(FNC_DWORD dwIndex, FNC_FILETIME *lpDateAccessed);
FNC_API FNC_DWORD fnc_GetResultAttributes(FNC_DWORD dwIndex); /* 0xFFFFFFFF on error */
FNC_API const FNC_WCHAR *fnc_GetResultFileListFileNameW(FNC_DWORD dwIndex);
FNC_API const char *fnc_GetResultFileListFileNameA(FNC_DWORD dwIndex);
FNC_API FNC_DWORD fnc_GetResultRunCount(FNC_DWORD dwIndex);
FNC_API FNC_BOOL fnc_GetResultDateRun(FNC_DWORD dwIndex, FNC_FILETIME *lpDateRun);
FNC_API FNC_BOOL fnc_GetResultDateRecentlyChanged(FNC_DWORD dwIndex, FNC_FILETIME *lpDateRecentlyChanged);
/* Highlighted text: '*' toggles highlighting, "**" is a literal '*'. */
FNC_API const FNC_WCHAR *fnc_GetResultHighlightedFileNameW(FNC_DWORD dwIndex);
FNC_API const char *fnc_GetResultHighlightedFileNameA(FNC_DWORD dwIndex);
FNC_API const FNC_WCHAR *fnc_GetResultHighlightedPathW(FNC_DWORD dwIndex);
FNC_API const char *fnc_GetResultHighlightedPathA(FNC_DWORD dwIndex);
FNC_API const FNC_WCHAR *fnc_GetResultHighlightedFullPathAndFileNameW(FNC_DWORD dwIndex);
FNC_API const char *fnc_GetResultHighlightedFullPathAndFileNameA(FNC_DWORD dwIndex);

/* control */
FNC_API void fnc_Reset(void);
FNC_API void fnc_CleanUp(void);
FNC_API FNC_DWORD fnc_GetMajorVersion(void); /* version reported to Everything clients */
FNC_API FNC_DWORD fnc_GetMinorVersion(void);
FNC_API FNC_DWORD fnc_GetRevision(void);
FNC_API FNC_DWORD fnc_GetBuildNumber(void);
FNC_API FNC_DWORD fnc_GetTargetMachine(void);
FNC_API FNC_BOOL fnc_IsDBLoaded(void);
FNC_API FNC_BOOL fnc_RebuildDB(void);
FNC_API FNC_BOOL fnc_UpdateAllFolderIndexes(void);
FNC_API FNC_BOOL fnc_SaveDB(void);
FNC_API FNC_BOOL fnc_SaveRunHistory(void);
FNC_API FNC_BOOL fnc_DeleteRunHistory(void);
FNC_API FNC_BOOL fnc_Exit(void);
FNC_API FNC_BOOL fnc_IsFastSort(FNC_DWORD dwSortType);
FNC_API FNC_BOOL fnc_IsFileInfoIndexed(FNC_DWORD dwFileInfoType);
FNC_API FNC_DWORD fnc_GetRunCountFromFileNameW(const FNC_WCHAR *lpFileName);
FNC_API FNC_BOOL fnc_SetRunCountFromFileNameW(const FNC_WCHAR *lpFileName, FNC_DWORD dwRunCount);
FNC_API FNC_DWORD fnc_IncRunCountFromFileNameW(const FNC_WCHAR *lpFileName);

#ifdef __cplusplus
}
#endif

#endif /* FENNEC_SDK_H */
