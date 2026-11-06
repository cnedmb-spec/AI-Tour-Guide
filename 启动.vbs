' Qiaoxiang AI Guide - one-click start (no console window)
' 1) health-check /api/stats first: if OUR service is up, just open the browser
' 2) if the port is taken by ANOTHER program, report it instead of opening the page
' 3) otherwise locate Python (PATH first, known path fallback), start Ollama + server,
'    wait for the health check, then open the browser
Option Explicit
On Error Resume Next

Dim sh, fso, base, py, url, i, logs, ex, out, lines, cand, j, ollama
Set sh = CreateObject("WScript.Shell")
Set fso = CreateObject("Scripting.FileSystemObject")

base = Left(WScript.ScriptFullName, InStrRev(WScript.ScriptFullName, "\") - 1)
url = "http://127.0.0.1:8765/"
logs = base & "\server\server.log"

' ---- locate python (DEF-22): PATH first, hardcoded path only as fallback ----
py = ""
Set ex = sh.Exec("cmd /c where python")
out = ex.StdOut.ReadAll
lines = Split(Replace(out, vbCr, ""), vbLf)
For j = 0 To UBound(lines)
  If InStr(lines(j), "WindowsApps") = 0 And InStr(LCase(lines(j)), "python.exe") > 0 Then
    If fso.FileExists(Trim(lines(j))) Then
      py = Trim(lines(j))
      Exit For
    End If
  End If
Next
If py = "" Then
  cand = sh.ExpandEnvironmentStrings("%LOCALAPPDATA%") & "\Programs\Python\Python311\python.exe"
  If fso.FileExists(cand) Then py = cand
End If
If py = "" Then
  MsgBox "未找到 Python。" & vbCrLf & _
         "请安装 Python（勾选加入 PATH），或编辑本脚本修正路径。", 16, "AI Guide - setup"
  WScript.Quit
End If

' ---- already running? use a real health check, not just the port ----
If ServiceUp() Then
  sh.Run url, 1, False
  WScript.Quit
End If

' ---- port busy but health check failed: another program owns the port ----
If PortInUse(8765) Then
  MsgBox "端口 8765 已被其他程序占用，本服务无法启动。" & vbCrLf & vbCrLf & _
         "请结束占用该端口的进程后重试（netstat -ano 可查 PID）。", 16, "AI Guide - start failed"
  WScript.Quit
End If

' ---- local LLM (tray app, keep quiet); skip silently if not installed ----
ollama = sh.ExpandEnvironmentStrings("%LOCALAPPDATA%") & "\Programs\Ollama\ollama app.exe"
If fso.FileExists(ollama) Then sh.Run """" & ollama & """", 1, False

' ---- guide server: hidden, detached, log to file ----
sh.CurrentDirectory = base & "\server"
sh.Run "cmd /c """ & py & """ -m uvicorn main:app --host 127.0.0.1 --port 8765 >> """ & logs & """ 2>&1", 0, False

' ---- wait for the health check (max 90s) ----
For i = 1 To 90
  WScript.Sleep 1000
  If ServiceUp() Then
    WScript.Sleep 1500
    sh.Run url, 1, False
    WScript.Quit
  End If
Next

MsgBox "服务未能在 90 秒内启动。" & vbCrLf & vbCrLf & _
       "1. 查看日志: server\server.log" & vbCrLf & _
       "2. 端口 8765 可能被占用" & vbCrLf & _
       "3. 手动测试: python -m uvicorn main:app --port 8765", 16, "AI Guide - start failed"

' ---- helpers ----
Function PortInUse(p)
  Dim e, o
  PortInUse = False
  On Error Resume Next
  Set e = sh.Exec("cmd /c netstat -ano -p tcp | findstr LISTENING")
  o = e.StdOut.ReadAll
  If InStr(o, ":" & p & " ") > 0 Then PortInUse = True
  On Error GoTo 0
End Function

' 真正调一次 /api/stats：端口通且返回 200 才算「我们的服务已就绪」，
' 避免端口被其他程序占用时误判成服务已在跑（DEF-22）
Function ServiceUp()
  Dim x
  ServiceUp = False
  On Error Resume Next
  Set x = CreateObject("MSXML2.ServerXMLHTTP.6.0")
  x.open "GET", "http://127.0.0.1:8765/api/stats", False
  x.setTimeouts 800, 800, 1500, 1500
  x.send
  If Err.Number = 0 Then
    If x.status = 200 Then ServiceUp = True
  End If
  On Error GoTo 0
End Function
