"""Supply generated process identities to the production Rust audio watchdog."""
import ctypes as ct


def synthetic_probe(discover, direct):
    typ = ct.CFUNCTYPE(ct.c_int, ct.c_char_p, ct.POINTER(ct.c_uint32), ct.POINTER(ct.c_double), ct.POINTER(ct.c_uint32))
    @typ
    def probe(ids, pid, created, edition):
        if not ids:
            return 4
        target = discover()
        if target is None:
            return 0
        pid[0], created[0], edition[0] = target[0], target[1], 1
        return 3 if direct() else 1
    return probe
