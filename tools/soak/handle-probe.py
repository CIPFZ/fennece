"""Handle-step probe for the REL-05 soak (Windows, no Sysinternals needed).

Polls one process every few seconds and logs, whenever its handles jump (>= 8) or a
DLL is loaded or unloaded: the change by object type, every new handle with its name
(registry keys, mutexes, sections, events, disk files; Thread handles with the name of
the thread they refer to), the modules loaded or unloaded, and the threads that started
or ended (with their names and start module). Once a minute it logs the handle, thread
and module counts with private bytes and the working set.

    python handle-probe.py <pid> <log file> [interval seconds, default 5]

Stop it by creating `<log file>.stop`. Run it elevated (or as SYSTEM) to read handles
of an elevated fennec. It found the REL-05 handle steps: shell icon calls on the
thumbnail threads (iconcache mutexes, known-folder keys, namespace extensions).
"""

import collections
import ctypes
import ctypes.wintypes as w
import datetime
import json
import os
import sys
import threading
import time

ntdll = ctypes.WinDLL('ntdll')
k32 = ctypes.WinDLL('kernel32', use_last_error=True)
psapi = ctypes.WinDLL('psapi')
k32.OpenProcess.restype = w.HANDLE
k32.GetCurrentProcess.restype = w.HANDLE
k32.OpenThread.restype = w.HANDLE
k32.CreateToolhelp32Snapshot.restype = w.HANDLE
k32.DuplicateHandle.argtypes = [w.HANDLE, w.HANDLE, w.HANDLE, ctypes.POINTER(w.HANDLE), w.DWORD, w.BOOL, w.DWORD]
k32.GetFileType.argtypes = [w.HANDLE]
k32.GetThreadId.argtypes = [w.HANDLE]
k32.CloseHandle.argtypes = [w.HANDLE]
k32.GetThreadDescription.argtypes = [w.HANDLE, ctypes.POINTER(ctypes.c_wchar_p)]
ntdll.NtQueryObject.argtypes = [w.HANDLE, ctypes.c_int, ctypes.c_void_p, w.ULONG, ctypes.POINTER(w.ULONG)]
ntdll.NtQueryInformationThread.argtypes = [w.HANDLE, ctypes.c_int, ctypes.c_void_p, w.ULONG, ctypes.POINTER(w.ULONG)]

PID = int(sys.argv[1])
OUT = sys.argv[2]
STEP = int(sys.argv[3]) if len(sys.argv) > 3 else 5
STOP = OUT + '.stop'
# PROCESS_DUP_HANDLE | PROCESS_QUERY_INFORMATION | PROCESS_VM_READ
hproc = k32.OpenProcess(0x0040 | 0x0400 | 0x0010, False, PID)
log = open(OUT, 'a', encoding='utf-8')


def say(*a):
    print(datetime.datetime.now().strftime('%H:%M:%S'), *a, file=log, flush=True)


class HandleEntry(ctypes.Structure):
    """SYSTEM_HANDLE_TABLE_ENTRY_INFO_EX."""
    _fields_ = [('Object', ctypes.c_void_p), ('UniqueProcessId', ctypes.c_size_t), ('HandleValue', ctypes.c_size_t),
                ('GrantedAccess', w.ULONG), ('CreatorBackTraceIndex', w.USHORT), ('ObjectTypeIndex', w.USHORT),
                ('HandleAttributes', w.ULONG), ('Reserved', w.ULONG)]


def object_types():
    """Object type index -> name (ObjectTypesInformation)."""
    b = ctypes.create_string_buffer(1 << 16)
    r = w.ULONG()
    ntdll.NtQueryObject(None, 3, b, 1 << 16, ctypes.byref(r))
    n = ctypes.c_ulong.from_buffer(b, 0).value
    off, out = 8, {}
    for i in range(n):
        ln = ctypes.c_ushort.from_buffer(b, off).value
        mx = ctypes.c_ushort.from_buffer(b, off + 2).value
        out[i + 2] = ctypes.wstring_at(ctypes.addressof(b) + off + 0x68, ln // 2)
        off += 0x68 + ((mx + 7) & ~7)
    return out


TYPES = object_types()
_size = [1 << 24]


def handles():
    """Handle value -> type name, for the probed process (SystemExtendedHandleInformation)."""
    while True:
        buf = ctypes.create_string_buffer(_size[0])
        ret = w.ULONG()
        st = ntdll.NtQuerySystemInformation(64, buf, _size[0], ctypes.byref(ret)) & 0xFFFFFFFF
        if st != 0xC0000004:
            break
        _size[0] *= 2
    n = ctypes.c_size_t.from_buffer(buf, 0).value
    arr = (HandleEntry * n).from_buffer(buf, 16)
    return {e.HandleValue: TYPES.get(e.ObjectTypeIndex, str(e.ObjectTypeIndex)) for e in arr if e.UniqueProcessId == PID}


NAMED = {'Key', 'Mutant', 'Section', 'Event', 'Semaphore', 'File', 'Directory', 'Thread'}
THREAD_NAMES = {}


def name_sync(hv, typ):
    d = w.HANDLE()
    if not k32.DuplicateHandle(hproc, w.HANDLE(hv), k32.GetCurrentProcess(), ctypes.byref(d), 0, False, 2):
        return '?dup'
    try:
        if typ == 'File' and k32.GetFileType(d) != 1:
            return '(not a disk file)'
        if typ == 'Thread':
            tid = k32.GetThreadId(d)
            return 'tid %d %s' % (tid, THREAD_NAMES.get(tid, ''))
        b = ctypes.create_string_buffer(4096)
        r = w.ULONG()
        if ntdll.NtQueryObject(d, 1, b, 4096, ctypes.byref(r)) != 0:
            return '?'
        ln = ctypes.c_ushort.from_buffer(b, 0).value
        return ctypes.wstring_at(ctypes.c_void_p.from_buffer(b, 8).value, ln // 2) if ln else ''
    finally:
        k32.CloseHandle(d)


def name_of(hv, typ):
    if typ not in NAMED:
        return ''
    if typ != 'File':
        return name_sync(hv, typ)
    # A file handle with synchronous I/O pending (a pipe) blocks every query: ask on a
    # thread and give up after 0.3 s.
    res = ['(timeout)']

    def run():
        res[0] = name_sync(hv, typ)

    t = threading.Thread(target=run, daemon=True)
    t.start()
    t.join(0.3)
    return res[0]


class ModuleInfo(ctypes.Structure):
    _fields_ = [('base', ctypes.c_void_p), ('size', w.DWORD), ('entry', ctypes.c_void_p)]


def modules():
    arr = (w.HMODULE * 2048)()
    need = w.DWORD()
    psapi.EnumProcessModulesEx(hproc, arr, ctypes.sizeof(arr), ctypes.byref(need), 3)
    out = {}
    for i in range(need.value // ctypes.sizeof(w.HMODULE)):
        nm = ctypes.create_unicode_buffer(512)
        psapi.GetModuleFileNameExW(hproc, w.HMODULE(arr[i]), nm, 512)
        mi = ModuleInfo()
        psapi.GetModuleInformation(hproc, w.HMODULE(arr[i]), ctypes.byref(mi), ctypes.sizeof(mi))
        out[os.path.basename(nm.value)] = (mi.base or 0, mi.size)
    return out


class ThreadEntry(ctypes.Structure):
    _fields_ = [('dwSize', w.DWORD), ('cntUsage', w.DWORD), ('th32ThreadID', w.DWORD), ('th32OwnerProcessID', w.DWORD),
                ('tpBasePri', w.LONG), ('tpDeltaPri', w.LONG), ('dwFlags', w.DWORD)]


def threads(mods):
    """Thread id -> (name, start module+offset)."""
    snap = k32.CreateToolhelp32Snapshot(4, 0)
    te = ThreadEntry()
    te.dwSize = ctypes.sizeof(ThreadEntry)
    out = {}
    ok = k32.Thread32First(snap, ctypes.byref(te))
    while ok:
        if te.th32OwnerProcessID == PID:
            tid = te.th32ThreadID
            h = k32.OpenThread(0x0040 | 0x0800, False, tid)
            nm, start = '', 0
            if h:
                p = ctypes.c_wchar_p()
                if k32.GetThreadDescription(h, ctypes.byref(p)) >= 0 and p.value:
                    nm = p.value
                a = ctypes.c_void_p()
                r = w.ULONG()
                ntdll.NtQueryInformationThread(h, 9, ctypes.byref(a), 8, ctypes.byref(r))
                start = a.value or 0
                k32.CloseHandle(h)
            where = hex(start)
            for mn, (b, s) in mods.items():
                if b <= start < b + s:
                    where = '%s+0x%x' % (mn, start - b)
                    break
            out[tid] = (nm, where)
            THREAD_NAMES[tid] = nm
        ok = k32.Thread32Next(snap, ctypes.byref(te))
    k32.CloseHandle(snap)
    return out


class MemoryCounters(ctypes.Structure):
    _fields_ = [('cb', w.DWORD), ('PageFaultCount', w.DWORD)] + [(n, ctypes.c_size_t) for n in (
        'PeakWorkingSetSize', 'WorkingSetSize', 'QuotaPeakPagedPoolUsage', 'QuotaPagedPoolUsage',
        'QuotaPeakNonPagedPoolUsage', 'QuotaNonPagedPoolUsage', 'PagefileUsage', 'PeakPagefileUsage', 'PrivateUsage')]


def main():
    say('probe pid', PID)
    prev_m = modules()
    prev_t = threads(prev_m)
    prev_h = handles()
    names = {hv: name_of(hv, ty) for hv, ty in prev_h.items()}
    say('start handles', len(prev_h), json.dumps(collections.Counter(prev_h.values()).most_common()))
    say('threads', json.dumps(sorted(prev_t.values())))
    last_print = 0.0
    while not os.path.exists(STOP):
        time.sleep(STEP)
        m = modules()
        t = threads(m)
        h = handles()
        if not h:
            say('process gone')
            break
        added = {hv: ty for hv, ty in h.items() if prev_h.get(hv) != ty}
        removed = {hv: ty for hv, ty in prev_h.items() if h.get(hv) != ty}
        new_mods, gone_mods = sorted(set(m) - set(prev_m)), sorted(set(prev_m) - set(m))
        new_thr = {k: v for k, v in t.items() if k not in prev_t}
        gone_thr = {k: v for k, v in prev_t.items() if k not in t}
        if len(added) >= 8 or new_mods or gone_mods or abs(len(h) - len(prev_h)) >= 8:
            cnt = collections.Counter(added.values())
            cnt.subtract(collections.Counter(removed.values()))
            say('STEP handles %d -> %d net %s' % (len(prev_h), len(h), json.dumps({k: v for k, v in cnt.items() if v})))
            for hv, ty in sorted(added.items()):
                names[hv] = name_of(hv, ty)
                say('   + %s %s %s' % (hex(hv), ty, names[hv]))
            for hv, ty in sorted(removed.items()):
                say('   - %s %s %s' % (hex(hv), ty, names.get(hv, '')))
            for mm in new_mods:
                say('   + module', mm)
            for mm in gone_mods:
                say('   - module', mm)
            for k, v in new_thr.items():
                say('   + thread', k, v)
            for k, v in gone_thr.items():
                say('   - thread', k, v)
        else:
            for hv, ty in added.items():
                names[hv] = name_of(hv, ty)
        if time.time() - last_print > 60:
            last_print = time.time()
            pmc = MemoryCounters()
            pmc.cb = ctypes.sizeof(MemoryCounters)
            psapi.GetProcessMemoryInfo(hproc, ctypes.byref(pmc), pmc.cb)
            say('handles', len(h), 'threads', len(t), 'modules', len(m),
                'private_mb %.1f ws_mb %.1f' % (pmc.PrivateUsage / 1048576, pmc.WorkingSetSize / 1048576))
        prev_h, prev_m, prev_t = h, m, t
    say('end')


main()
