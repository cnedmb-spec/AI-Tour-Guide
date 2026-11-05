' Qiaoxiang AI Guide - public link (runs the tunnel in background, no console)
' Double-click this file. The URL is written to "PUBLIC-URL.txt" next to this script.
' Single-instance: if a cloudflared tunnel is already running, reuse the saved URL
' instead of starting a second unauthenticated tunnel (DEF-23).
Option Explicit

Dim sh, fso, base, exe, log, txt, s, i, url, re, wmi, procs, p, f, running
Set sh = CreateObject("WScript.Shell")
Set fso = CreateObject("Scripting.FileSystemObject")
base = Left(WScript.ScriptFullName, InStrRev(WScript.ScriptFullName, "\") - 1)
exe = base & "\tools\cloudflared.exe"
log = base & "\cf.log"
txt = base & "\PUBLIC-URL.txt"

If Not fso.FileExists(exe) Then
  MsgBox "tools\cloudflared.exe not found", 16, "Public link"
  WScript.Quit 1
End If

' ---- single instance check: reuse the existing tunnel if one is alive ----
running = 0
On Error Resume Next
Set wmi = GetObject("winmgmts:\\.\root\cimv2")
Set procs = wmi.ExecQuery("select ProcessId from Win32_Process where Name='cloudflared.exe'")
For Each p In procs
  running = running + 1
Next
On Error GoTo 0

If running > 0 Then
  If fso.FileExists(txt) Then
    s = fso.OpenTextFile(txt, 1).ReadAll
    If InStr(s, "trycloudflare.com") > 0 Then
      sh.Run Trim(Replace(Replace(s, vbCr, ""), vbLf, "")), 1, False
      WScript.Quit
    End If
  End If
  MsgBox "已有 cloudflared 隧道在运行，但未找到可用地址。" & vbCrLf & _
         "请先结束 cloudflared 进程再重新双击本脚本。", 48, "Public link"
  WScript.Quit
End If

If fso.FileExists(log) Then fso.DeleteFile log
If fso.FileExists(txt) Then fso.DeleteFile txt

' 0 = hidden window, False = do not wait (the sleep loop below keeps it alive)
sh.Run """" & exe & """ tunnel --url http://127.0.0.1:8765 --no-autoupdate --logfile """ & log & """ --loglevel info", 0, False

Set re = New RegExp
re.Pattern = "https://[a-z0-9-]+\.trycloudflare\.com"
re.IgnoreCase = True
url = ""
For i = 1 To 45
  WScript.Sleep 1000
  If fso.FileExists(log) Then
    On Error Resume Next
    s = fso.OpenTextFile(log, 1).ReadAll
    On Error GoTo 0
    If re.Test(s) Then
      url = re.Execute(s)(0).Value
      Exit For
    End If
  End If
Next

If url = "" Then
  MsgBox "Tunnel did not start. Check the network and try again.", 16, "Public link"
  WScript.Quit 1
End If

' 以 ASCII 写出（无 BOM），避免程序读取时把 BOM 拼进 URL
Set f = fso.CreateTextFile(txt, True, False)
f.WriteLine url
f.Close

' stay resident so the tunnel process is not reclaimed
Do
  WScript.Sleep 60000
Loop
