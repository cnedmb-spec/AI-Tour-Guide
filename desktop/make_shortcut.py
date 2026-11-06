# -*- coding: utf-8 -*-
"""在桌面生成标准 Windows 快捷方式（.lnk）。

使用 Windows 原生的 IShellLink / IPersistFile COM 接口（pywin32），由 Shell
自己写入完整的 LinkTargetIDList，因此双击一定能被解析。手写二进制 lnk 缺
IDList 时会被判为「无关联应用」(WinError 1155)，已废弃该做法。
本脚本不调用 WScript.Shell。

用法：python make_shortcut.py [快捷方式名]
"""
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TARGET = ROOT / "QiaoxiangGuide.exe"
ICON = ROOT / "desktop" / "assets" / "guide.ico"
DESKTOP = Path.home() / "Desktop"
NAME = sys.argv[1] if len(sys.argv) > 1 else "侨乡AI讲解员"


def main() -> int:
    for label, p in (("exe", TARGET), ("icon", ICON)):
        if not p.is_file():
            print(f"ERROR: {label} 不存在：{p}")
            return 1
    if not DESKTOP.is_dir():
        print("ERROR: 桌面目录不存在：", DESKTOP)
        return 1

    import pythoncom
    from win32com.shell import shell

    out = DESKTOP / f"{NAME}.lnk"
    pythoncom.CoInitialize()
    try:
        link = pythoncom.CoCreateInstance(
            shell.CLSID_ShellLink,
            None,
            pythoncom.CLSCTX_INPROC_SERVER,
            shell.IID_IShellLink,
        )
        link.SetPath(str(TARGET))
        link.SetWorkingDirectory(str(ROOT))
        link.SetIconLocation(str(ICON), 0)
        link.SetDescription(NAME)
        link.SetShowCmd(1)  # SW_SHOWNORMAL
        link.QueryInterface(pythoncom.IID_IPersistFile).Save(str(out), 0)
    finally:
        pythoncom.CoUninitialize()

    print("shortcut:", out)
    print("target  :", TARGET)
    print("workdir :", ROOT)
    print("icon    :", ICON)
    print("bytes   :", out.stat().st_size)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
