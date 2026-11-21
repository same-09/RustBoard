"""Import guards for the AI stack.

transformers imports torchvision whenever it looks installed, even though RVC
never uses it. A torchvision that doesn't match torch ("operator
torchvision::nms does not exist") or is half-installed ("cannot import name
'io' from 'torchvision'") then crashes the HuBERT import. We don't need
torchvision, so if it's unusable we hide it from transformers instead.
"""
from __future__ import annotations

import sys
import warnings

TORCHVISION_BROKEN = False


def _hide(err: Exception) -> None:
    global TORCHVISION_BROKEN
    for name in [m for m in sys.modules if m == "torchvision" or m.startswith("torchvision.")]:
        del sys.modules[name]
    sys.modules["torchvision"] = None  # find_spec() -> None, so transformers treats it as absent
    TORCHVISION_BROKEN = True
    warnings.warn(f"ignoring unusable torchvision ({err}). Run fix_ai.bat to repair it.", stacklevel=3)


def hide_broken_torchvision() -> None:
    if "transformers" in sys.modules:
        return  # too late to influence its availability checks
    try:
        # exactly what transformers pulls in; a half-installed torchvision can
        # import fine at the top level and still be missing these
        import torchvision  # noqa: F401
        from torchvision import io  # noqa: F401
        from torchvision.transforms import InterpolationMode  # noqa: F401
    except ModuleNotFoundError as e:
        if e.name != "torchvision":  # not installed at all is fine; a missing piece is not
            _hide(e)
    except Exception as e:  # noqa: BLE001 - any other failure also means unusable
        _hide(e)


hide_broken_torchvision()
