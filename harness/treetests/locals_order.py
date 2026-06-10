"""Module docstring."""
import logging
from os.path import join, dirname


def __virtual__():
    global _dscl, _flush_dscl_cache
    _dscl = 1
    _flush_dscl_cache = 2


def determine():
    global AppKit, Foundation
    import AppKit
    import Foundation


def set_clip():
    global copy, paste
    copy = 1
    paste = 2


def windows():
    global HGLOBAL, LPVOID
    from ctypes.wintypes import HGLOBAL, LPVOID
