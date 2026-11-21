"""Offline mode: while it's on, RustBoard can't open any internet connection.

This works at the lowest level Python offers, not by promising to skip downloads:
every outgoing socket connection and every DNS lookup to a non-local address is
refused, and proxies are switched off (a proxy on this PC would otherwise forward
"local" connections to the internet). A firewall (Portmaster) or a packet capture
(Wireshark) sees nothing. Connections to this PC itself (localhost) still work.

Turn it on in the app (routing panel, "Offline mode") or for any start with the
environment variable RUSTBOARD_OFFLINE=1.
"""
from __future__ import annotations

import ipaddress
import os
import socket
import sys
import urllib.request

ENV = "RUSTBOARD_OFFLINE"
# libraries that check these themselves and use their local cache only
LIB_ENV = ("HF_HUB_OFFLINE", "TRANSFORMERS_OFFLINE", "HF_DATASETS_OFFLINE")
MESSAGE = ("Offline mode is on, so RustBoard can't connect to the internet. "
           "Turn it off in the routing panel (click the device row at the top) to download.")

_orig = {
    "connect": socket.socket.connect,
    "connect_ex": socket.socket.connect_ex,
    "create_connection": socket.create_connection,
    "getaddrinfo": socket.getaddrinfo,
}
_on = False
# A proxy running on this PC would forward our "local" connections to the internet,
# so offline mode also switches every proxy setting off (and puts it back after).
PROXY_ENV = ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy",
             "NO_PROXY", "no_proxy")
_saved_env: dict = {}
_orig_getproxies = urllib.request.getproxies


class OfflineError(ConnectionRefusedError):
    def __init__(self, what: str = ""):
        super().__init__(f"{what + ': ' if what else ''}{MESSAGE}")


def _is_local(host) -> bool:
    if host is None:
        return True                      # bind/listen on this machine
    if isinstance(host, bytes):
        host = host.decode(errors="ignore")
    host = str(host).strip("[]").split("%")[0].lower()
    if host in ("", "localhost", "localhost.localdomain") or host.endswith(".localhost"):
        return True
    try:
        return ipaddress.ip_address(host).is_loopback
    except ValueError:
        return False                      # a host name: would need DNS, i.e. the internet


def _host_of(address):
    if isinstance(address, (tuple, list)) and address:
        return address[0]
    return None                           # AF_UNIX paths and similar: local by nature


def _connect(self, address):
    if not _is_local(_host_of(address)):
        raise OfflineError()
    return _orig["connect"](self, address)


def _connect_ex(self, address):
    if not _is_local(_host_of(address)):
        raise OfflineError()
    return _orig["connect_ex"](self, address)


def _create_connection(address, *args, **kwargs):
    if not _is_local(_host_of(address)):
        raise OfflineError()
    return _orig["create_connection"](address, *args, **kwargs)


def _getaddrinfo(host, *args, **kwargs):
    if not _is_local(host):
        raise OfflineError()                # no DNS lookups either
    return _orig["getaddrinfo"](host, *args, **kwargs)


def is_offline() -> bool:
    return _on


def set_offline(on: bool) -> None:
    """Block (or allow again) every outgoing connection from this process.

    Also sets the environment for libraries and for child processes (training),
    so they use what's already downloaded instead of trying to connect."""
    global _on
    _on = bool(on)
    if _on:
        if not _saved_env:
            _saved_env.update({k: os.environ[k] for k in PROXY_ENV if k in os.environ})
        for k in PROXY_ENV:
            os.environ.pop(k, None)
        os.environ["NO_PROXY"] = os.environ["no_proxy"] = "*"
        urllib.request.getproxies = lambda: {}
        urllib.request._opener = None          # forget an opener built with the old proxies
        socket.socket.connect = _connect
        socket.socket.connect_ex = _connect_ex
        socket.create_connection = _create_connection
        socket.getaddrinfo = _getaddrinfo
        for k in LIB_ENV:
            os.environ[k] = "1"
        os.environ[ENV] = "1"
    else:
        # the methods come from the C base class: removing ours restores it exactly
        for name in ("connect", "connect_ex"):
            if name in vars(socket.socket):
                delattr(socket.socket, name)
        socket.create_connection = _orig["create_connection"]
        socket.getaddrinfo = _orig["getaddrinfo"]
        for k in LIB_ENV + (ENV,) + PROXY_ENV:
            os.environ.pop(k, None)
        os.environ.update(_saved_env)
        _saved_env.clear()
        urllib.request.getproxies = _orig_getproxies
        urllib.request._opener = None


def from_env() -> bool:
    return os.environ.get(ENV, "").strip() not in ("", "0")


def clear_persistent() -> bool:
    """Remove a persistent user-level RUSTBOARD_OFFLINE (Windows), so offline mode
    doesn't switch itself back on at the next launch after the user turns it off.

    Only touches our own variable under HKCU\\Environment. Returns True if one was
    there and removed. No-op (returns False) off Windows or if it isn't set."""
    if sys.platform != "win32":
        return False
    try:
        import winreg
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, "Environment", 0,
                            winreg.KEY_SET_VALUE | winreg.KEY_QUERY_VALUE) as key:
            try:
                winreg.QueryValueEx(key, ENV)
            except OSError:
                return False                   # not set persistently: nothing to do
            winreg.DeleteValue(key, ENV)
        return True
    except OSError:
        return False


def require_online(what: str) -> None:
    """Call before something that can only work online (searching, listing a repo)."""
    if _on:
        raise OfflineError(what)


def is_offline_error(text: str) -> bool:
    """Does an error message come from offline mode (ours, or a library's offline check)?"""
    t = text or ""
    return (MESSAGE in t or "HF_HUB_OFFLINE" in t or "LocalEntryNotFoundError" in t
            or "offline mode is enabled" in t.lower() or "Cannot reach https://huggingface.co" in t)
