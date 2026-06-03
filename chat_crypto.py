#!/usr/bin/env python3
"""Local chat storage crypto for AgoraLink.

This module is intentionally independent from the RUDP transport session keys.
Transport encryption protects packets on the network; this module protects
message bodies after they are stored in the local SQLite database.
"""

from __future__ import annotations

import base64
import json
import os
from typing import Dict, Tuple

from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from cryptography.hazmat.primitives.kdf.scrypt import Scrypt

STORAGE_KDF_NAME = "scrypt"
STORAGE_KEY_LEN = 32
STORAGE_KDF_N = 2 ** 15
STORAGE_KDF_R = 8
STORAGE_KDF_P = 1
BODY_ALG = "AES-256-GCM"
NONCE_LEN = 12


def b64e(data: bytes) -> str:
    return base64.b64encode(bytes(data or b"")).decode("ascii")


def b64d(text: str) -> bytes:
    return base64.b64decode(str(text or "").encode("ascii"))


def new_kdf_config() -> Dict[str, object]:
    return {
        "kdf": STORAGE_KDF_NAME,
        "salt": b64e(os.urandom(16)),
        "n": STORAGE_KDF_N,
        "r": STORAGE_KDF_R,
        "p": STORAGE_KDF_P,
        "key_len": STORAGE_KEY_LEN,
    }


def derive_storage_key(password: str, config: Dict[str, object]) -> bytes:
    if not str(password or ""):
        raise ValueError("empty_chat_storage_password")
    if str(config.get("kdf") or "") != STORAGE_KDF_NAME:
        raise ValueError("unsupported_chat_storage_kdf")
    salt = b64d(str(config.get("salt") or ""))
    n = int(config.get("n") or STORAGE_KDF_N)
    r = int(config.get("r") or STORAGE_KDF_R)
    p = int(config.get("p") or STORAGE_KDF_P)
    length = int(config.get("key_len") or STORAGE_KEY_LEN)
    kdf = Scrypt(salt=salt, length=length, n=n, r=r, p=p)
    return kdf.derive(str(password).encode("utf-8"))


def make_aad(fields: Dict[str, object]) -> bytes:
    # Deterministic JSON keeps authenticated metadata stable across process runs.
    return json.dumps(fields or {}, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")


def encrypt_json_body(storage_key: bytes, body: Dict[str, object], aad_fields: Dict[str, object]) -> Tuple[bytes, bytes, str]:
    if len(bytes(storage_key or b"")) != STORAGE_KEY_LEN:
        raise ValueError("invalid_storage_key_length")
    nonce = os.urandom(NONCE_LEN)
    plaintext = json.dumps(body or {}, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")
    ciphertext = AESGCM(bytes(storage_key)).encrypt(nonce, plaintext, make_aad(aad_fields))
    return ciphertext, nonce, BODY_ALG


def decrypt_json_body(storage_key: bytes, ciphertext: bytes, nonce: bytes, aad_fields: Dict[str, object]) -> Dict[str, object]:
    if len(bytes(storage_key or b"")) != STORAGE_KEY_LEN:
        raise ValueError("invalid_storage_key_length")
    plaintext = AESGCM(bytes(storage_key)).decrypt(bytes(nonce), bytes(ciphertext), make_aad(aad_fields))
    obj = json.loads(plaintext.decode("utf-8"))
    if not isinstance(obj, dict):
        raise ValueError("chat_body_not_object")
    return obj


def encrypt_verifier(storage_key: bytes) -> Dict[str, str]:
    nonce = os.urandom(NONCE_LEN)
    ct = AESGCM(bytes(storage_key)).encrypt(nonce, b"agoralink-chat-storage-verifier-v1", b"agoralink")
    return {"nonce": b64e(nonce), "ciphertext": b64e(ct), "alg": BODY_ALG}


def verify_storage_key(storage_key: bytes, verifier: Dict[str, object]) -> None:
    nonce = b64d(str(verifier.get("nonce") or ""))
    ct = b64d(str(verifier.get("ciphertext") or ""))
    pt = AESGCM(bytes(storage_key)).decrypt(nonce, ct, b"agoralink")
    if pt != b"agoralink-chat-storage-verifier-v1":
        raise ValueError("chat_storage_password_verification_failed")
