"""Discover official GitHub releases and verify signed, bounded Windows downloads."""
import base64
import binascii
import ctypes
from ctypes import wintypes as wt
from dataclasses import dataclass
from functools import cache
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import time
from urllib.parse import urlsplit
import urllib.request

from release_identity import PUBLIC_KEY

REPO = "https://github.com/lumberjackfrog/Lummix/releases"
FEED = "https://api.github.com/repos/lumberjackfrog/Lummix/releases?per_page=30"
ASSET = "Lummix.Setup.exe"
MANIFEST = "Lummix.Update.json"
LIMIT = 128 * 1024 * 1024
CHUNK = 128 * 1024
VERSION_RE = re.compile(r"(?:0|[1-9][0-9]{0,3})(?:\.(?:0|[1-9][0-9]{0,3})){2}")
HOSTS = {"api.github.com", "github.com", "release-assets.githubusercontent.com", "objects.githubusercontent.com", "github-releases.githubusercontent.com"}


class InvalidUpdate(ValueError):
    pass


def safe_url(url):
    p = urlsplit(url)
    return p.scheme == "https" and p.netloc in HOSTS and not p.fragment


class GithubRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, message, headers, newurl):
        if not safe_url(newurl):
            raise InvalidUpdate("redirect")
        return super().redirect_request(req, fp, code, message, headers, newurl)


def open_url(url):
    if not safe_url(url):
        raise InvalidUpdate("origin")
    request = urllib.request.Request(url, headers={"User-Agent":"Lummix-Windows-Updater", "Accept-Encoding":"identity"})
    return urllib.request.build_opener(GithubRedirect()).open(request, timeout=15)


def read_json(url, limit, opener):
    with opener(url) as response:
        raw = response.read(limit + 1)
    if len(raw) > limit:
        raise InvalidUpdate("metadata size")
    try:
        return json.loads(raw)
    except (UnicodeError, ValueError) as error:
        raise InvalidUpdate("metadata") from error


@cache
def bcrypt():
    # Keep one DLL reference. Per-verification algorithm and key handles are released below.
    api = ctypes.WinDLL("bcrypt")
    handle = ctypes.c_void_p
    api.BCryptOpenAlgorithmProvider.argtypes = [ctypes.POINTER(handle), wt.LPCWSTR, wt.LPCWSTR, wt.ULONG]
    api.BCryptImportKeyPair.argtypes = [handle, handle, wt.LPCWSTR, ctypes.POINTER(handle), ctypes.c_char_p, wt.ULONG, wt.ULONG]
    api.BCryptVerifySignature.argtypes = [handle, handle, ctypes.c_char_p, wt.ULONG, ctypes.c_char_p, wt.ULONG, wt.ULONG]
    api.BCryptDestroyKey.argtypes = [handle]
    api.BCryptCloseAlgorithmProvider.argtypes = [handle, wt.ULONG]
    return api


def verify_signature(data, signature, public_key=PUBLIC_KEY):
    # Windows CNG returns NTSTATUS. Its P-256 signatures are fixed-size r || s.
    key_blob = base64.b64decode(public_key, validate=True)
    if len(signature) != 64 or len(key_blob) != 72:
        raise InvalidUpdate("signature size")
    if getattr(sys, "frozen", False):
        host = Path(sys.executable).resolve().with_name("Lummix Host.exe")
        args = [base64.b64encode(value).decode("ascii") for value in (key_blob, data, signature)]
        result = subprocess.run([str(host), "verify-update", *args], creationflags=subprocess.CREATE_NO_WINDOW,
                                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if result.returncode:
            raise InvalidUpdate("signature verification")
        return
    api = bcrypt()
    alg, key = ctypes.c_void_p(), ctypes.c_void_p()
    if api.BCryptOpenAlgorithmProvider(ctypes.byref(alg),"ECDSA_P256",None,0) != 0:
        raise InvalidUpdate("signature provider")
    try:
        if api.BCryptImportKeyPair(alg,None,"ECCPUBLICBLOB",ctypes.byref(key),key_blob,len(key_blob),0) != 0:
            raise InvalidUpdate("verification key")
        digest = hashlib.sha256(data).digest()
        if api.BCryptVerifySignature(key,None,digest,len(digest),signature,len(signature),0) != 0:
            raise InvalidUpdate("signature mismatch")
    finally:
        if key: api.BCryptDestroyKey(key)
        api.BCryptCloseAlgorithmProvider(alg,0)


def version_number(value):
    if not isinstance(value,str) or not VERSION_RE.fullmatch(value):
        return ()
    return tuple(map(int,value.split(".")))


@dataclass(frozen=True)
class Release:
    version: str
    size: int
    digest: str

    @property
    def url(self):
        return f"{REPO}/download/v{self.version}/{ASSET}"


def signed_release(document, tag, size, digest, verifier=verify_signature):
    if (not isinstance(document,dict) or type(document.get("format")) is not int or document["format"]!=1
            or not version_number(tag.removeprefix("v")) or not tag.startswith("v")
            or document.get("tag")!=tag or type(size) is not int or not 0<size<=LIMIT
            or type(document.get("size")) is not int or document["size"]!=size
            or not isinstance(digest,str) or not re.fullmatch(r"[a-f0-9]{64}",digest) or document.get("sha256")!=digest):
        raise InvalidUpdate("release identity")
    try:
        signature = base64.b64decode(document["signature"], validate=True)
    except (KeyError, TypeError, ValueError, binascii.Error) as error:
        raise InvalidUpdate("signature encoding") from error
    data = f"Lummix Windows x64 update v1\n{tag}\n{size}\n{digest}\n".encode("ascii")
    verifier(data, signature)
    return Release(tag[1:],size,digest)


def latest(current, opener=open_url):
    releases = read_json(FEED, 1024*1024, opener)
    if not isinstance(releases,list):
        raise InvalidUpdate("release list")
    candidates = []
    for release in releases:
        if not isinstance(release,dict):
            raise InvalidUpdate("release row")
        tag = release.get("tag_name")
        if not isinstance(tag,str) or not tag.startswith("v") or not version_number(tag[1:]):
            continue
        if release.get("draft") is False and release.get("published_at") and version_number(tag[1:]) > version_number(current):
            candidates.append((version_number(tag[1:]),release))
    if not candidates:
        return None
    release = max(candidates,key=lambda row:row[0])[1]
    return from_release(release,opener)


def from_release(release, opener=open_url, verifier=verify_signature):
    tag = release["tag_name"]
    if (not isinstance(tag,str) or not tag.startswith("v") or not version_number(tag[1:])
            or release.get("draft") is not False or not release.get("published_at")
            or release.get("html_url") != f"{REPO}/tag/{tag}"):
        raise InvalidUpdate("release URL")
    prefix = f"{REPO}/download/{tag}/"
    assets = {}
    for asset in release.get("assets",[]):
        name = asset.get("name")
        if name in (ASSET,MANIFEST) and asset.get("state")=="uploaded":
            if name in assets or asset.get("browser_download_url")!=prefix+name:
                raise InvalidUpdate("asset URL")
            assets[name] = asset
    if set(assets)!={ASSET,MANIFEST} or type(assets[MANIFEST].get("size")) is not int or not 0<assets[MANIFEST]["size"]<=8192:
        raise InvalidUpdate("missing signed release")
    exe = assets[ASSET]
    digest = exe.get("digest") or ""
    if not isinstance(digest,str) or not digest.startswith("sha256:"):
        raise InvalidUpdate("asset digest")
    return signed_release(read_json(prefix+MANIFEST,8192,opener),tag,exe.get("size"),digest[7:],verifier)


def plain(path):
    if path.exists() and getattr(path.lstat(),"st_file_attributes",0) & stat.FILE_ATTRIBUTE_REPARSE_POINT:
        raise InvalidUpdate("unexpected cache link")


def download(release, directory, cancelled, opener=open_url):
    directory = Path(directory)
    plain(directory)
    directory.mkdir(exist_ok=True,parents=True)
    final, partial = directory / ASSET, directory / "download.part"
    for path in (final, partial):
        plain(path)
        path.unlink(missing_ok=True)
    digest = hashlib.sha256()
    total = 0
    deadline = time.monotonic()+600
    try:
        with opener(release.url) as response, partial.open("xb") as out:
            while not cancelled.is_set():
                chunk = response.read(CHUNK)
                if not chunk: break
                total += len(chunk)
                if total > release.size or time.monotonic()>deadline:
                    raise InvalidUpdate("download limit")
                digest.update(chunk)
                out.write(chunk)
        if cancelled.is_set(): raise InterruptedError("closed")
        if total!=release.size or digest.hexdigest()!=release.digest:
            raise InvalidUpdate("download mismatch")
        # Preserve Windows Internet-zone checks when the user opens the downloaded EXE.
        with open(str(partial)+":Zone.Identifier","w",encoding="ascii",newline="") as zone:
            zone.write(f"[ZoneTransfer]\r\nZoneId=3\r\nHostUrl={release.url}\r\n")
        partial.replace(final)
        return final
    finally:
        partial.unlink(missing_ok=True)
