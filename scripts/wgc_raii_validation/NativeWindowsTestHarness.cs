using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

internal static class NativeWindowsTestHarness
{
    private const uint CreateNewProcessGroup = 0x00000200;
    private const uint CreateUnicodeEnvironment = 0x00000400;
    private const uint StartfUseStdHandles = 0x00000100;
    private const uint CtrlBreakEvent = 1;
    private const uint WaitObject0 = 0;
    private const uint WaitTimeout = 258;
    private const uint GenericRead = 0x80000000;
    private const uint GenericWrite = 0x40000000;
    private const uint FileShareRead = 0x00000001;
    private const uint FileShareWrite = 0x00000002;
    private const uint CreateAlways = 2;
    private const uint OpenExisting = 3;
    private const uint FileAttributeNormal = 0x00000080;
    private const uint WmClose = 0x0010;

    [StructLayout(LayoutKind.Sequential)]
    private struct SecurityAttributes
    {
        internal int Length;
        internal IntPtr SecurityDescriptor;
        [MarshalAs(UnmanagedType.Bool)] internal bool InheritHandle;
    }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct StartupInfo
    {
        internal int Size;
        internal IntPtr Reserved;
        internal IntPtr Desktop;
        internal IntPtr Title;
        internal int X;
        internal int Y;
        internal int XSize;
        internal int YSize;
        internal int XCountChars;
        internal int YCountChars;
        internal int FillAttribute;
        internal int Flags;
        internal short ShowWindow;
        internal short Reserved2;
        internal IntPtr Reserved2Pointer;
        internal IntPtr StandardInput;
        internal IntPtr StandardOutput;
        internal IntPtr StandardError;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct ProcessInformation
    {
        internal IntPtr Process;
        internal IntPtr Thread;
        internal uint ProcessId;
        internal uint ThreadId;
    }

    private delegate bool EnumWindowsCallback(IntPtr window, IntPtr parameter);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern bool CreateProcessW(
        string applicationName,
        StringBuilder commandLine,
        IntPtr processAttributes,
        IntPtr threadAttributes,
        bool inheritHandles,
        uint creationFlags,
        IntPtr environment,
        string currentDirectory,
        ref StartupInfo startupInfo,
        out ProcessInformation processInformation);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr CreateFileW(
        string path,
        uint desiredAccess,
        uint shareMode,
        ref SecurityAttributes securityAttributes,
        uint creationDisposition,
        uint flagsAndAttributes,
        IntPtr templateFile);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool GenerateConsoleCtrlEvent(uint controlEvent, uint processGroupId);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern uint WaitForSingleObject(IntPtr handle, uint milliseconds);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool GetExitCodeProcess(IntPtr process, out uint exitCode);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool CloseHandle(IntPtr handle);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool EnumWindows(EnumWindowsCallback callback, IntPtr parameter);

    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(IntPtr window, out uint processId);

    [DllImport("user32.dll")]
    private static extern bool IsWindowVisible(IntPtr window);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetWindowTextW(IntPtr window, StringBuilder text, int capacity);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool PostMessageW(IntPtr window, uint message, IntPtr wParam, IntPtr lParam);

    private sealed class WindowSearch
    {
        internal uint ProcessId;
        internal string TitleContains;
        internal IntPtr Window;
        internal string Title;
    }

    private static int Main(string[] args)
    {
        try
        {
            if (args.Length == 0)
                throw new ArgumentException("expected ctrl-break or wm-close mode");
            if (args[0] == "ctrl-break")
                return RunCtrlBreak(args);
            if (args[0] == "wm-close")
                return RunWindowClose(args);
            throw new ArgumentException("unknown mode: " + args[0]);
        }
        catch (Exception error)
        {
            Console.Error.WriteLine(error.ToString());
            Console.WriteLine("{\"success\":false,\"error\":\"" + Escape(error.Message) + "\"}");
            return 2;
        }
    }

    private static int RunCtrlBreak(string[] args)
    {
        string executable = Required(args, "--exe");
        string stdoutPath = Required(args, "--stdout");
        string stderrPath = Required(args, "--stderr");
        int delayMs = ParseInt(Required(args, "--delay-ms"), "delay-ms", 0, 60000);
        int timeoutMs = ParseInt(Required(args, "--timeout-ms"), "timeout-ms", 1000, 120000);
        int separator = Array.IndexOf(args, "--");
        if (separator < 0)
            throw new ArgumentException("ctrl-break requires -- before child arguments");
        var childArguments = new List<string>();
        for (int index = separator + 1; index < args.Length; index++)
            childArguments.Add(args[index]);

        Directory.CreateDirectory(Path.GetDirectoryName(Path.GetFullPath(stdoutPath)));
        Directory.CreateDirectory(Path.GetDirectoryName(Path.GetFullPath(stderrPath)));
        var security = new SecurityAttributes {
            Length = Marshal.SizeOf(typeof(SecurityAttributes)),
            SecurityDescriptor = IntPtr.Zero,
            InheritHandle = true,
        };
        IntPtr stdout = CreateFileW(stdoutPath, GenericWrite, FileShareRead | FileShareWrite,
            ref security, CreateAlways, FileAttributeNormal, IntPtr.Zero);
        IntPtr stderr = CreateFileW(stderrPath, GenericWrite, FileShareRead | FileShareWrite,
            ref security, CreateAlways, FileAttributeNormal, IntPtr.Zero);
        IntPtr stdin = CreateFileW("NUL", GenericRead, FileShareRead | FileShareWrite,
            ref security, OpenExisting, FileAttributeNormal, IntPtr.Zero);
        if (stdout == new IntPtr(-1) || stderr == new IntPtr(-1) || stdin == new IntPtr(-1))
            throw new Win32Exception(Marshal.GetLastWin32Error(), "failed to create redirected handles");

        var startup = new StartupInfo {
            Size = Marshal.SizeOf(typeof(StartupInfo)),
            Flags = (int)StartfUseStdHandles,
            StandardInput = stdin,
            StandardOutput = stdout,
            StandardError = stderr,
        };
        ProcessInformation process;
        var command = new StringBuilder(Quote(executable));
        foreach (string argument in childArguments)
            command.Append(' ').Append(Quote(argument));
        bool started = CreateProcessW(executable, command, IntPtr.Zero, IntPtr.Zero, true,
            CreateNewProcessGroup | CreateUnicodeEnvironment, IntPtr.Zero,
            Path.GetDirectoryName(Path.GetFullPath(executable)), ref startup, out process);
        int startError = Marshal.GetLastWin32Error();
        CloseHandle(stdin);
        CloseHandle(stdout);
        CloseHandle(stderr);
        if (!started)
            throw new Win32Exception(startError, "CreateProcessW failed");

        CloseHandle(process.Thread);
        Thread.Sleep(delayMs);
        bool signalSent = GenerateConsoleCtrlEvent(CtrlBreakEvent, process.ProcessId);
        int signalError = signalSent ? 0 : Marshal.GetLastWin32Error();
        uint wait = WaitForSingleObject(process.Process, (uint)timeoutMs);
        bool timedOut = wait == WaitTimeout;
        bool secondSignalSent = false;
        if (timedOut)
        {
            secondSignalSent = GenerateConsoleCtrlEvent(CtrlBreakEvent, process.ProcessId);
            wait = WaitForSingleObject(process.Process, 3000);
        }
        uint exitCode = UInt32.MaxValue;
        bool exited = wait == WaitObject0 && GetExitCodeProcess(process.Process, out exitCode);
        CloseHandle(process.Process);

        Console.WriteLine(
            "{\"success\":" + Bool(signalSent && exited && !timedOut) +
            ",\"pid\":" + process.ProcessId +
            ",\"control_event\":\"CTRL_BREAK_EVENT\"" +
            ",\"control_event_sent\":" + Bool(signalSent) +
            ",\"control_event_error\":" + signalError +
            ",\"timed_out\":" + Bool(timedOut) +
            ",\"second_signal_sent\":" + Bool(secondSignalSent) +
            ",\"exited\":" + Bool(exited) +
            ",\"exit_code\":" + (exited ? exitCode.ToString() : "null") + "}");
        return signalSent && exited && !timedOut ? 0 : 1;
    }

    private static int RunWindowClose(string[] args)
    {
        uint processId = (uint)ParseInt(Required(args, "--pid"), "pid", 1, Int32.MaxValue);
        string titleContains = Optional(args, "--title-contains") ?? String.Empty;
        int timeoutMs = ParseInt(Optional(args, "--timeout-ms") ?? "10000", "timeout-ms", 100, 60000);
        DateTime deadline = DateTime.UtcNow.AddMilliseconds(timeoutMs);
        var search = new WindowSearch { ProcessId = processId, TitleContains = titleContains };
        while (DateTime.UtcNow < deadline && search.Window == IntPtr.Zero)
        {
            EnumWindows(delegate(IntPtr window, IntPtr parameter) {
                uint owner;
                GetWindowThreadProcessId(window, out owner);
                if (owner != search.ProcessId || !IsWindowVisible(window))
                    return true;
                var title = new StringBuilder(512);
                GetWindowTextW(window, title, title.Capacity);
                if (search.TitleContains.Length == 0 || title.ToString().IndexOf(search.TitleContains, StringComparison.OrdinalIgnoreCase) >= 0)
                {
                    search.Window = window;
                    search.Title = title.ToString();
                    return false;
                }
                return true;
            }, IntPtr.Zero);
            if (search.Window == IntPtr.Zero)
                Thread.Sleep(25);
        }
        bool posted = search.Window != IntPtr.Zero && PostMessageW(search.Window, WmClose, IntPtr.Zero, IntPtr.Zero);
        int error = posted ? 0 : Marshal.GetLastWin32Error();
        Console.WriteLine(
            "{\"success\":" + Bool(posted) +
            ",\"pid\":" + processId +
            ",\"hwnd\":" + (search.Window == IntPtr.Zero ? "null" : "\"0x" + search.Window.ToInt64().ToString("X") + "\"") +
            ",\"title\":\"" + Escape(search.Title ?? String.Empty) + "\"" +
            ",\"wm_close_posted\":" + Bool(posted) +
            ",\"win32_error\":" + error + "}");
        return posted ? 0 : 1;
    }

    private static string Required(string[] args, string name)
    {
        string value = Optional(args, name);
        if (value == null)
            throw new ArgumentException("missing " + name);
        return value;
    }

    private static string Optional(string[] args, string name)
    {
        for (int index = 1; index + 1 < args.Length; index++)
            if (args[index] == name)
                return args[index + 1];
        return null;
    }

    private static int ParseInt(string text, string name, int minimum, int maximum)
    {
        int value;
        if (!Int32.TryParse(text, out value) || value < minimum || value > maximum)
            throw new ArgumentException(name + " is outside " + minimum + ".." + maximum);
        return value;
    }

    private static string Quote(string value)
    {
        if (value.Length > 0 && value.IndexOfAny(new[] { ' ', '\t', '\n', '\v', '\"' }) < 0)
            return value;
        var output = new StringBuilder("\"");
        int backslashes = 0;
        foreach (char character in value)
        {
            if (character == '\\')
            {
                backslashes++;
            }
            else if (character == '\"')
            {
                output.Append('\\', backslashes * 2 + 1).Append('\"');
                backslashes = 0;
            }
            else
            {
                output.Append('\\', backslashes).Append(character);
                backslashes = 0;
            }
        }
        output.Append('\\', backslashes * 2).Append('\"');
        return output.ToString();
    }

    private static string Bool(bool value) { return value ? "true" : "false"; }

    private static string Escape(string value)
    {
        return value.Replace("\\", "\\\\").Replace("\"", "\\\"")
            .Replace("\r", "\\r").Replace("\n", "\\n");
    }
}
