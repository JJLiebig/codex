"""Observe Windows processes; never stop or restart them."""

import ctypes as c
from ctypes import wintypes as w


kernel = c.WinDLL("kernel32", use_last_error=True)
restart = c.WinDLL("rstrtmgr", use_last_error=True)


def api(library, name, result, *arguments):
    function = getattr(library, name)
    function.restype, function.argtypes = result, arguments
    return function


class Process(c.Structure):
    _fields_ = [
        ("size", w.DWORD),
        ("usage", w.DWORD),
        ("pid", w.DWORD),
        ("heap", c.c_size_t),
        ("module", w.DWORD),
        ("threads", w.DWORD),
        ("parent", w.DWORD),
        ("priority", w.LONG),
        ("flags", w.DWORD),
        ("name", w.WCHAR * 260),
    ]


class Identity(c.Structure):
    _fields_ = [("pid", w.DWORD), ("started", w.FILETIME)]


class ResourceProcess(c.Structure):
    _fields_ = [
        ("identity", Identity),
        ("name", w.WCHAR * 256),
        ("service", w.WCHAR * 64),
        ("kind", w.DWORD),
        ("status", w.ULONG),
        ("session", w.DWORD),
        ("restartable", w.BOOL),
    ]


close = api(kernel, "CloseHandle", w.BOOL, w.HANDLE)
open_process = api(kernel, "OpenProcess", w.HANDLE, w.DWORD, w.BOOL, w.DWORD)
times = api(kernel, "GetProcessTimes", w.BOOL, w.HANDLE, *([c.POINTER(w.FILETIME)] * 4))
snapshot = api(kernel, "CreateToolhelp32Snapshot", w.HANDLE, w.DWORD, w.DWORD)
first = api(kernel, "Process32FirstW", w.BOOL, w.HANDLE, c.POINTER(Process))
next_process = api(kernel, "Process32NextW", w.BOOL, w.HANDLE, c.POINTER(Process))
start = api(restart, "RmStartSession", w.DWORD, c.POINTER(w.DWORD), w.DWORD, w.LPWSTR)
end = api(restart, "RmEndSession", w.DWORD, w.DWORD)
register = api(
    restart,
    "RmRegisterResources",
    w.DWORD,
    w.DWORD,
    w.UINT,
    c.POINTER(w.LPCWSTR),
    w.UINT,
    c.c_void_p,
    w.UINT,
    c.c_void_p,
)
get_list = api(
    restart,
    "RmGetList",
    w.DWORD,
    w.DWORD,
    c.POINTER(w.UINT),
    c.POINTER(w.UINT),
    c.POINTER(ResourceProcess),
    c.POINTER(w.DWORD),
)


def ticks(value):
    return value.dwLowDateTime | value.dwHighDateTime << 32


def identity(pid):
    handle = open_process(0x1000, False, pid)
    if not handle:
        if c.get_last_error() == 87:  # Process exited between enumeration and open.
            return None
        raise c.WinError(c.get_last_error())
    try:
        values = [w.FILETIME() for _ in range(4)]
        if not times(handle, *map(c.byref, values)):
            raise c.WinError(c.get_last_error())
        return None if ticks(values[1]) else (pid, ticks(values[0]))
    finally:
        close(handle)


def codex_processes():
    handle = snapshot(2, 0)
    if handle == c.c_void_p(-1).value:
        raise c.WinError(c.get_last_error())
    result = set()
    try:
        process = Process()
        process.size = c.sizeof(process)
        found = first(handle, c.byref(process))
        while found:
            name = process.name.lower()
            if name.startswith("codex") and name.endswith(".exe"):
                value = identity(process.pid)
                if value:
                    result.add(value)
            found = next_process(handle, c.byref(process))
        if c.get_last_error() != 18:  # ERROR_NO_MORE_FILES
            raise c.WinError(c.get_last_error())
        return result
    finally:
        close(handle)


def database_users(path):
    session, key = w.DWORD(), c.create_unicode_buffer(33)
    status = start(c.byref(session), 0, key)
    if status:
        raise c.WinError(status)
    try:
        paths = [
            str(p)
            for suffix in ("", "-wal", "-shm")
            if (p := path.with_name(path.name + suffix)).exists()
        ]
        files = (w.LPCWSTR * len(paths))(*paths)
        status = register(session, len(paths), files, 0, None, 0, None)
        if status:
            raise c.WinError(status)
        needed, count, reasons = w.UINT(), w.UINT(), w.DWORD()
        while True:
            rows = (ResourceProcess * count.value)()
            status = get_list(
                session, c.byref(needed), c.byref(count), rows, c.byref(reasons)
            )
            if status == 234:
                count.value = needed.value
                continue
            if status or reasons.value:
                raise OSError("Cannot establish history process ownership")
            return {
                (row.identity.pid, ticks(row.identity.started))
                for row in rows[: count.value]
            }
    finally:
        end(session)
