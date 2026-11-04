// Native helpers for bench-vs-we.ps1, compiled at run time by Add-Type (PowerShell 7, no
// install). Everything here is black-box observation through public Windows APIs:
// process list and times, memory counters, DXGI adapters, top-level windows, and the
// Windows Restart Manager (the API installers use to close running applications).
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

namespace LumenwallBench
{
    public sealed class ProcInfo
    {
        public int Pid;
        public int ParentPid;
        public string Name;
        /// <summary>Creation time (FILETIME, UTC), 0 when the process cannot be opened.</summary>
        public long CreateTime;
        /// <summary>Kernel + user CPU time, 100 ns units.</summary>
        public long CpuTime;
        public long PrivateBytes;
        public long WorkingSet;
        public bool Opened;
    }

    public sealed class AdapterInfo
    {
        public string Description;
        public uint VendorId;
        /// <summary>LUID formatted like the GPU counter instance names: 0xHHHHHHHH_0xLLLLLLLL.</summary>
        public string Luid;
        public long DedicatedVideoMemory;
        public bool Software;
        public List<string> Outputs = new List<string>();
    }

    public static class Native
    {
        // ---------------------------------------------------------------- processes

        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
        struct PROCESSENTRY32W
        {
            public uint dwSize;
            public uint cntUsage;
            public uint th32ProcessID;
            public IntPtr th32DefaultHeapID;
            public uint th32ModuleID;
            public uint cntThreads;
            public uint th32ParentProcessID;
            public int pcPriClassBase;
            public uint dwFlags;
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 260)]
            public string szExeFile;
        }

        [StructLayout(LayoutKind.Sequential)]
        struct PROCESS_MEMORY_COUNTERS_EX
        {
            public uint cb;
            public uint PageFaultCount;
            public UIntPtr PeakWorkingSetSize;
            public UIntPtr WorkingSetSize;
            public UIntPtr QuotaPeakPagedPoolUsage;
            public UIntPtr QuotaPagedPoolUsage;
            public UIntPtr QuotaPeakNonPagedPoolUsage;
            public UIntPtr QuotaNonPagedPoolUsage;
            public UIntPtr PagefileUsage;
            public UIntPtr PeakPagefileUsage;
            public UIntPtr PrivateUsage;
        }

        const uint TH32CS_SNAPPROCESS = 0x2;
        const uint PROCESS_QUERY_LIMITED_INFORMATION = 0x1000;
        const uint PROCESS_VM_READ = 0x10;

        [DllImport("kernel32.dll", SetLastError = true)]
        static extern IntPtr CreateToolhelp32Snapshot(uint flags, uint pid);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        static extern bool Process32FirstW(IntPtr snapshot, ref PROCESSENTRY32W entry);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        static extern bool Process32NextW(IntPtr snapshot, ref PROCESSENTRY32W entry);
        [DllImport("kernel32.dll")]
        static extern bool CloseHandle(IntPtr handle);
        [DllImport("kernel32.dll", SetLastError = true)]
        static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);
        [DllImport("kernel32.dll", SetLastError = true)]
        static extern bool GetProcessTimes(IntPtr process, out long creation, out long exit, out long kernel, out long user);
        [DllImport("kernel32.dll", EntryPoint = "K32GetProcessMemoryInfo", SetLastError = true)]
        static extern bool GetProcessMemoryInfo(IntPtr process, out PROCESS_MEMORY_COUNTERS_EX counters, uint size);
        [DllImport("kernel32.dll", SetLastError = true)]
        static extern bool GetSystemTimes(out long idle, out long kernel, out long user);

        /// <summary>Every process: pid, parent pid and executable name (no handle needed).</summary>
        public static List<ProcInfo> Snapshot()
        {
            var list = new List<ProcInfo>(512);
            IntPtr snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if (snap == IntPtr.Zero || snap == new IntPtr(-1)) return list;
            try
            {
                var e = new PROCESSENTRY32W();
                e.dwSize = (uint)Marshal.SizeOf(typeof(PROCESSENTRY32W));
                if (!Process32FirstW(snap, ref e)) return list;
                do
                {
                    list.Add(new ProcInfo { Pid = (int)e.th32ProcessID, ParentPid = (int)e.th32ParentProcessID, Name = e.szExeFile });
                } while (Process32NextW(snap, ref e));
            }
            finally { CloseHandle(snap); }
            return list;
        }

        /// <summary>Fills creation time, CPU time and memory of one process.</summary>
        public static void Fill(ProcInfo p)
        {
            IntPtr h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, false, (uint)p.Pid);
            if (h == IntPtr.Zero) h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, (uint)p.Pid);
            if (h == IntPtr.Zero) return;
            try
            {
                long c, x, k, u;
                if (GetProcessTimes(h, out c, out x, out k, out u))
                {
                    p.CreateTime = c;
                    p.CpuTime = k + u;
                    p.Opened = true;
                }
                PROCESS_MEMORY_COUNTERS_EX m;
                if (GetProcessMemoryInfo(h, out m, (uint)Marshal.SizeOf(typeof(PROCESS_MEMORY_COUNTERS_EX))))
                {
                    p.PrivateBytes = (long)m.PrivateUsage.ToUInt64();
                    p.WorkingSet = (long)m.WorkingSetSize.ToUInt64();
                }
            }
            finally { CloseHandle(h); }
        }

        /// <summary>Creation time of a process (FILETIME), 0 when it cannot be opened.</summary>
        public static long CreationTime(int pid)
        {
            var p = new ProcInfo { Pid = pid };
            Fill(p);
            return p.CreateTime;
        }

        /// <summary>System-wide CPU times: [idle, kernel (includes idle), user], 100 ns units.</summary>
        public static long[] SystemTimes()
        {
            long i, k, u;
            GetSystemTimes(out i, out k, out u);
            return new long[] { i, k, u };
        }

        // ---------------------------------------------------------------- DXGI adapters

        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
        struct DXGI_ADAPTER_DESC1
        {
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)]
            public string Description;
            public uint VendorId;
            public uint DeviceId;
            public uint SubSysId;
            public uint Revision;
            public UIntPtr DedicatedVideoMemory;
            public UIntPtr DedicatedSystemMemory;
            public UIntPtr SharedSystemMemory;
            public uint LuidLow;
            public int LuidHigh;
            public uint Flags;
        }

        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
        struct DXGI_OUTPUT_DESC
        {
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)]
            public string DeviceName;
            public int Left, Top, Right, Bottom;
            public int AttachedToDesktop;
            public int Rotation;
            public IntPtr Monitor;
        }

        // Only the vtable slots up to the methods used are declared; the placeholders are
        // never called.
        [ComImport, Guid("ae02eedb-c735-4690-8d52-5a8dc20213aa"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
        interface IDXGIOutput
        {
            void SetPrivateData(); void SetPrivateDataInterface(); void GetPrivateData(); void GetParent();
            [PreserveSig] int GetDesc(out DXGI_OUTPUT_DESC desc);
        }

        [ComImport, Guid("29038f61-3839-4626-91fd-086879011a05"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
        interface IDXGIAdapter1
        {
            void SetPrivateData(); void SetPrivateDataInterface(); void GetPrivateData(); void GetParent();
            [PreserveSig] int EnumOutputs(uint index, out IDXGIOutput output);
            void GetDesc(); void CheckInterfaceSupport();
            [PreserveSig] int GetDesc1(out DXGI_ADAPTER_DESC1 desc);
        }

        [ComImport, Guid("770aae78-f26f-4dba-a829-253c83d1b387"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
        interface IDXGIFactory1
        {
            void SetPrivateData(); void SetPrivateDataInterface(); void GetPrivateData(); void GetParent();
            void EnumAdapters(); void MakeWindowAssociation(); void GetWindowAssociation(); void CreateSwapChain(); void CreateSoftwareAdapter();
            [PreserveSig] int EnumAdapters1(uint index, out IDXGIAdapter1 adapter);
        }

        [DllImport("dxgi.dll")]
        static extern int CreateDXGIFactory1(ref Guid riid, [MarshalAs(UnmanagedType.Interface)] out IDXGIFactory1 factory);

        /// <summary>DXGI adapters with their LUID and the monitors they drive.</summary>
        public static List<AdapterInfo> Adapters()
        {
            var list = new List<AdapterInfo>();
            var iid = typeof(IDXGIFactory1).GUID;
            IDXGIFactory1 factory;
            if (CreateDXGIFactory1(ref iid, out factory) != 0) return list;
            for (uint i = 0; ; i++)
            {
                IDXGIAdapter1 adapter;
                if (factory.EnumAdapters1(i, out adapter) != 0) break;
                DXGI_ADAPTER_DESC1 d;
                if (adapter.GetDesc1(out d) == 0)
                {
                    var a = new AdapterInfo
                    {
                        Description = d.Description,
                        VendorId = d.VendorId,
                        Luid = string.Format("0x{0:X8}_0x{1:X8}", (uint)d.LuidHigh, d.LuidLow),
                        DedicatedVideoMemory = (long)d.DedicatedVideoMemory.ToUInt64(),
                        Software = (d.Flags & 2) != 0,
                    };
                    for (uint o = 0; ; o++)
                    {
                        IDXGIOutput output;
                        if (adapter.EnumOutputs(o, out output) != 0) break;
                        DXGI_OUTPUT_DESC od;
                        if (output.GetDesc(out od) == 0)
                            a.Outputs.Add(string.Format("{0} {1}x{2} at {3},{4}", od.DeviceName, od.Right - od.Left, od.Bottom - od.Top, od.Left, od.Top));
                        Marshal.ReleaseComObject(output);
                    }
                    list.Add(a);
                }
                Marshal.ReleaseComObject(adapter);
            }
            Marshal.ReleaseComObject(factory);
            return list;
        }

        // ---------------------------------------------------------------- displays

        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
        struct DISPLAY_DEVICE
        {
            public int cb;
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)] public string DeviceName;
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)] public string DeviceString;
            public int StateFlags;
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)] public string DeviceID;
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)] public string DeviceKey;
        }

        [DllImport("user32.dll", CharSet = CharSet.Unicode)]
        static extern bool EnumDisplayDevices(string device, uint index, ref DISPLAY_DEVICE info, uint flags);

        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
        struct DEVMODE
        {
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)] public string dmDeviceName;
            public short dmSpecVersion, dmDriverVersion, dmSize, dmDriverExtra;
            public int dmFields;
            public int dmPositionX, dmPositionY, dmDisplayOrientation, dmDisplayFixedOutput;
            public short dmColor, dmDuplex, dmYResolution, dmTTOption, dmCollate;
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)] public string dmFormName;
            public short dmLogPixels;
            public int dmBitsPerPel, dmPelsWidth, dmPelsHeight, dmDisplayFlags, dmDisplayFrequency;
            public int dmICMMethod, dmICMIntent, dmMediaType, dmDitherType, dmReserved1, dmReserved2, dmPanningWidth, dmPanningHeight;
        }

        [DllImport("user32.dll", CharSet = CharSet.Unicode)]
        static extern bool EnumDisplaySettings(string device, int mode, ref DEVMODE info);

        /// <summary>
        /// Displays attached to the desktop: "DISPLAYn|adapter that drives it|monitor device id|primary|WxH@Hz".
        /// The adapter is the one the monitor is plugged into, which can differ from the one
        /// DXGI lists the output under on a hybrid machine.
        /// </summary>
        public static List<string> DisplayDevices()
        {
            var list = new List<string>();
            for (uint i = 0; ; i++)
            {
                var d = new DISPLAY_DEVICE();
                d.cb = Marshal.SizeOf(typeof(DISPLAY_DEVICE));
                if (!EnumDisplayDevices(null, i, ref d, 0)) break;
                if ((d.StateFlags & 1) == 0) continue;
                var m = new DISPLAY_DEVICE();
                m.cb = Marshal.SizeOf(typeof(DISPLAY_DEVICE));
                string monitor = EnumDisplayDevices(d.DeviceName, 0, ref m, 0) ? m.DeviceID : "";
                var mode = new DEVMODE();
                mode.dmSize = (short)Marshal.SizeOf(typeof(DEVMODE));
                string current = EnumDisplaySettings(d.DeviceName, -1 /* ENUM_CURRENT_SETTINGS */, ref mode)
                    ? string.Format("{0}x{1}@{2}Hz", mode.dmPelsWidth, mode.dmPelsHeight, mode.dmDisplayFrequency) : "";
                list.Add(string.Format("{0}|{1}|{2}|{3}|{4}", d.DeviceName.Replace(@"\\.\", ""), d.DeviceString, monitor, (d.StateFlags & 4) != 0, current));
            }
            return list;
        }

        // ---------------------------------------------------------------- windows

        delegate bool EnumWindowsProc(IntPtr hwnd, IntPtr lParam);

        [StructLayout(LayoutKind.Sequential)]
        struct RECT { public int Left, Top, Right, Bottom; }

        [StructLayout(LayoutKind.Sequential)]
        struct MONITORINFO { public uint cbSize; public RECT rcMonitor; public RECT rcWork; public uint dwFlags; }

        [DllImport("user32.dll")] static extern bool EnumWindows(EnumWindowsProc cb, IntPtr lParam);
        [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
        [DllImport("user32.dll")] static extern bool IsZoomed(IntPtr hwnd);
        [DllImport("user32.dll")] static extern bool IsIconic(IntPtr hwnd);
        [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr hwnd, out RECT rect);
        [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
        [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr hwnd, StringBuilder text, int max);
        [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr hwnd, StringBuilder text, int max);
        [DllImport("user32.dll")] static extern IntPtr MonitorFromWindow(IntPtr hwnd, uint flags);
        [DllImport("user32.dll")] static extern bool GetMonitorInfo(IntPtr monitor, ref MONITORINFO info);
        [DllImport("dwmapi.dll")] static extern int DwmGetWindowAttribute(IntPtr hwnd, int attribute, out int value, int size);

        static readonly HashSet<string> ShellClasses = new HashSet<string>(StringComparer.OrdinalIgnoreCase)
        {
            "Progman", "WorkerW", "Shell_TrayWnd", "Shell_SecondaryTrayWnd",
        };

        /// <summary>
        /// Visible, uncloaked top-level windows that are maximized or cover a whole monitor,
        /// excluding the desktop shell and the processes in <paramref name="ignorePids"/>.
        /// Returns "pid|class|title|maximized|fullscreen" lines.
        /// </summary>
        public static List<string> CoveringWindows(int[] ignorePids)
        {
            var ignore = new HashSet<int>(ignorePids ?? new int[0]);
            var found = new List<string>();
            EnumWindows((h, l) =>
            {
                if (!IsWindowVisible(h) || IsIconic(h)) return true;
                int cloaked;
                if (DwmGetWindowAttribute(h, 14, out cloaked, 4) == 0 && cloaked != 0) return true;
                uint pid;
                GetWindowThreadProcessId(h, out pid);
                if (ignore.Contains((int)pid)) return true;
                var cls = new StringBuilder(256);
                GetClassName(h, cls, cls.Capacity);
                if (ShellClasses.Contains(cls.ToString())) return true;
                bool maximized = IsZoomed(h);
                bool fullscreen = false;
                RECT r;
                if (GetWindowRect(h, out r))
                {
                    var mi = new MONITORINFO();
                    mi.cbSize = (uint)Marshal.SizeOf(typeof(MONITORINFO));
                    if (GetMonitorInfo(MonitorFromWindow(h, 2), ref mi))
                        fullscreen = r.Left <= mi.rcMonitor.Left && r.Top <= mi.rcMonitor.Top
                            && r.Right >= mi.rcMonitor.Right && r.Bottom >= mi.rcMonitor.Bottom;
                }
                if (maximized || fullscreen)
                {
                    var title = new StringBuilder(256);
                    GetWindowText(h, title, title.Capacity);
                    found.Add(string.Format("{0}|{1}|{2}|{3}|{4}", pid, cls, title, maximized, fullscreen));
                }
                return true;
            }, IntPtr.Zero);
            return found;
        }

        /// <summary>Visible top-level windows of the given processes: "pid|class|title".</summary>
        public static List<string> WindowsOf(int[] pids)
        {
            var want = new HashSet<int>(pids ?? new int[0]);
            var found = new List<string>();
            EnumWindows((h, l) =>
            {
                uint pid;
                GetWindowThreadProcessId(h, out pid);
                if (!want.Contains((int)pid) || !IsWindowVisible(h)) return true;
                var cls = new StringBuilder(256);
                GetClassName(h, cls, cls.Capacity);
                var title = new StringBuilder(256);
                GetWindowText(h, title, title.Capacity);
                found.Add(string.Format("{0}|{1}|{2}", pid, cls, title));
                return true;
            }, IntPtr.Zero);
            return found;
        }

        // ---------------------------------------------------------------- audio sessions

        [ComImport, Guid("BCDE0395-E52F-467C-8E3D-C4579291692E")]
        class MMDeviceEnumeratorClass { }

        [ComImport, Guid("A95664D2-9614-4F35-A746-DE8DB63617E6"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
        interface IMMDeviceEnumerator
        {
            [PreserveSig] int EnumAudioEndpoints(int dataFlow, int stateMask, out IMMDeviceCollection devices);
        }

        [ComImport, Guid("0BD7A1BE-7A1A-44DB-8397-CC5392387B5E"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
        interface IMMDeviceCollection
        {
            [PreserveSig] int GetCount(out uint count);
            [PreserveSig] int Item(uint index, out IMMDevice device);
        }

        [ComImport, Guid("D666063F-1587-4E43-81F1-B948E807363F"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
        interface IMMDevice
        {
            [PreserveSig] int Activate(ref Guid iid, int clsCtx, IntPtr activationParams, [MarshalAs(UnmanagedType.IUnknown)] out object iface);
        }

        [ComImport, Guid("77AA99A0-1BD6-484F-8BC7-2C654C9A9B6F"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
        interface IAudioSessionManager2
        {
            void GetAudioSessionControl(); void GetSimpleAudioVolume();
            [PreserveSig] int GetSessionEnumerator(out IAudioSessionEnumerator sessions);
        }

        [ComImport, Guid("E2F5BB11-0570-40CA-ACDD-3AA01277DEE8"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
        interface IAudioSessionEnumerator
        {
            [PreserveSig] int GetCount(out int count);
            [PreserveSig] int GetSession(int index, [MarshalAs(UnmanagedType.IUnknown)] out object session);
        }

        [ComImport, Guid("BFB7FF88-7239-4FC9-8FA2-07C950BE9C6D"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
        interface IAudioSessionControl2
        {
            void GetState(); void GetDisplayName(); void SetDisplayName(); void GetIconPath(); void SetIconPath();
            void GetGroupingParam(); void SetGroupingParam(); void RegisterAudioSessionNotification(); void UnregisterAudioSessionNotification();
            void GetSessionIdentifier(); void GetSessionInstanceIdentifier();
            [PreserveSig] int GetProcessId(out uint pid);
        }

        [ComImport, Guid("C02216F6-8C67-4B5B-9D00-D008E73E0064"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
        interface IAudioMeterInformation
        {
            [PreserveSig] int GetPeakValue(out float peak);
        }

        /// <summary>
        /// Highest current peak (0..1) of the audio sessions of <paramref name="pids"/> on every
        /// active playback device; -1 when nothing could be read. Read-only: changes no volume.
        /// </summary>
        public static float SessionPeak(int[] pids)
        {
            var want = new HashSet<int>(pids ?? new int[0]);
            float best = -1f;
            try
            {
                var enumerator = (IMMDeviceEnumerator)new MMDeviceEnumeratorClass();
                IMMDeviceCollection devices;
                if (enumerator.EnumAudioEndpoints(0 /* eRender */, 1 /* ACTIVE */, out devices) != 0) return best;
                uint n;
                devices.GetCount(out n);
                var iid = typeof(IAudioSessionManager2).GUID;
                for (uint d = 0; d < n; d++)
                {
                    IMMDevice device;
                    if (devices.Item(d, out device) != 0) continue;
                    object o;
                    if (device.Activate(ref iid, 23 /* CLSCTX_ALL */, IntPtr.Zero, out o) != 0) continue;
                    var manager = (IAudioSessionManager2)o;
                    IAudioSessionEnumerator sessions;
                    if (manager.GetSessionEnumerator(out sessions) != 0) continue;
                    int count;
                    sessions.GetCount(out count);
                    if (best < 0) best = 0f;
                    for (int s = 0; s < count; s++)
                    {
                        object session;
                        if (sessions.GetSession(s, out session) != 0) continue;
                        uint pid;
                        if (((IAudioSessionControl2)session).GetProcessId(out pid) != 0 || !want.Contains((int)pid)) continue;
                        float peak;
                        if (((IAudioMeterInformation)session).GetPeakValue(out peak) == 0 && peak > best) best = peak;
                    }
                }
            }
            catch (Exception) { }
            return best;
        }

        // ---------------------------------------------------------------- NVML

        // NVML is the library nvidia-smi reports from (nvml.dll, installed with the driver).
        [StructLayout(LayoutKind.Sequential)] struct NvmlUtilization { public uint Gpu; public uint Memory; }
        // nvmlMemory_v2_t: "used" without the driver-reserved memory, as nvidia-smi shows it.
        [StructLayout(LayoutKind.Sequential)] struct NvmlMemory { public uint Version; public ulong Total; public ulong Reserved; public ulong Free; public ulong Used; }
        [DllImport("nvml.dll")] static extern int nvmlInit_v2();
        [DllImport("nvml.dll")] static extern int nvmlDeviceGetHandleByIndex_v2(uint index, out IntPtr device);
        [DllImport("nvml.dll")] static extern int nvmlDeviceGetPowerUsage(IntPtr device, out uint milliwatts);
        [DllImport("nvml.dll")] static extern int nvmlDeviceGetUtilizationRates(IntPtr device, out NvmlUtilization utilization);
        [DllImport("nvml.dll")] static extern int nvmlDeviceGetDecoderUtilization(IntPtr device, out uint utilization, out uint periodUs);
        [DllImport("nvml.dll")] static extern int nvmlDeviceGetClockInfo(IntPtr device, int clockType, out uint mhz);
        [DllImport("nvml.dll")] static extern int nvmlDeviceGetMemoryInfo_v2(IntPtr device, ref NvmlMemory memory);
        [DllImport("nvml.dll")] static extern int nvmlDeviceGetPerformanceState(IntPtr device, out int pstate);

        static IntPtr nvmlDevice = IntPtr.Zero;
        static bool nvmlTried;

        /// <summary>
        /// GPU 0: [power W, utilization %, decoder %, graphics MHz, memory MHz, memory used MiB,
        /// P-state]; null without NVML. NaN for a value the driver does not report.
        /// </summary>
        public static double[] NvmlRead()
        {
            try
            {
                if (!nvmlTried)
                {
                    nvmlTried = true;
                    if (nvmlInit_v2() == 0) { IntPtr d; if (nvmlDeviceGetHandleByIndex_v2(0, out d) == 0) nvmlDevice = d; }
                }
                if (nvmlDevice == IntPtr.Zero) return null;
                var r = new double[7];
                for (int i = 0; i < r.Length; i++) r[i] = double.NaN;
                uint v, period; NvmlUtilization u; NvmlMemory m; int p;
                if (nvmlDeviceGetPowerUsage(nvmlDevice, out v) == 0) r[0] = v / 1000.0;
                if (nvmlDeviceGetUtilizationRates(nvmlDevice, out u) == 0) r[1] = u.Gpu;
                if (nvmlDeviceGetDecoderUtilization(nvmlDevice, out v, out period) == 0) r[2] = v;
                if (nvmlDeviceGetClockInfo(nvmlDevice, 0, out v) == 0) r[3] = v;
                if (nvmlDeviceGetClockInfo(nvmlDevice, 2, out v) == 0) r[4] = v;
                m = new NvmlMemory { Version = (uint)Marshal.SizeOf(typeof(NvmlMemory)) | (2u << 24) };
                if (nvmlDeviceGetMemoryInfo_v2(nvmlDevice, ref m) == 0) r[5] = m.Used / 1048576.0;
                if (nvmlDeviceGetPerformanceState(nvmlDevice, out p) == 0) r[6] = p;
                return r;
            }
            catch (Exception) { return null; }
        }

        // ---------------------------------------------------------------- Restart Manager

        [StructLayout(LayoutKind.Sequential)]
        struct RM_UNIQUE_PROCESS
        {
            public int dwProcessId;
            public long ProcessStartTime;
        }

        [DllImport("rstrtmgr.dll", CharSet = CharSet.Unicode)]
        static extern int RmStartSession(out uint session, int flags, StringBuilder key);
        [DllImport("rstrtmgr.dll")]
        static extern int RmEndSession(uint session);
        [DllImport("rstrtmgr.dll", CharSet = CharSet.Unicode)]
        static extern int RmRegisterResources(uint session, uint nFiles, string[] files, uint nApps, [In] RM_UNIQUE_PROCESS[] apps, uint nServices, string[] services);
        [DllImport("rstrtmgr.dll")]
        static extern int RmShutdown(uint session, uint flags, IntPtr callback);

        /// <summary>
        /// Asks the processes to close through the Restart Manager, the way an installer does
        /// (WM_QUERYENDSESSION / WM_ENDSESSION with ENDSESSION_CLOSEAPP). No forced
        /// termination: returns the Win32 error code, 0 when every process agreed to close.
        /// </summary>
        public static int RestartManagerClose(int[] pids)
        {
            uint session;
            var key = new StringBuilder(64);
            int rc = RmStartSession(out session, 0, key);
            if (rc != 0) return rc;
            try
            {
                var apps = new List<RM_UNIQUE_PROCESS>();
                foreach (var pid in pids)
                {
                    long created = CreationTime(pid);
                    if (created != 0) apps.Add(new RM_UNIQUE_PROCESS { dwProcessId = pid, ProcessStartTime = created });
                }
                if (apps.Count == 0) return 0;
                rc = RmRegisterResources(session, 0, null, (uint)apps.Count, apps.ToArray(), 0, null);
                if (rc != 0) return rc;
                return RmShutdown(session, 0, IntPtr.Zero);
            }
            finally { RmEndSession(session); }
        }
    }
}
