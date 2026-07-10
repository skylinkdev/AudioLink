import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:shared_preferences/shared_preferences.dart';

void main() {
  runApp(const LanAudioApp());
}

const _themeModeKey = 'themeMode';
const _realtimeAudioCyanSeed = Color(0xff0891b2);
const _professionalDarkNeutralSeed = Color(0xff64748b);
const _darkThemeButtonBlue = Color(0xff7991d1);

class LanAudioApp extends StatefulWidget {
  const LanAudioApp({super.key});

  @override
  State<LanAudioApp> createState() => _LanAudioAppState();
}

class _LanAudioAppState extends State<LanAudioApp> {
  var _themeMode = ThemeMode.system;

  @override
  void initState() {
    super.initState();
    _loadThemeMode();
  }

  Future<void> _loadThemeMode() async {
    final prefs = await SharedPreferences.getInstance();
    final value = prefs.getString(_themeModeKey);
    if (!mounted) return;
    setState(() {
      _themeMode = _themeModeFromString(value);
    });
  }

  Future<void> _setThemeMode(ThemeMode themeMode) async {
    final prefs = await SharedPreferences.getInstance();
    await prefs.setString(_themeModeKey, themeMode.name);
    if (!mounted) return;
    setState(() {
      _themeMode = themeMode;
    });
  }

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Audio Link',
      themeMode: _themeMode,
      theme: _buildLanAudioTheme(
        seedColor: _realtimeAudioCyanSeed,
        brightness: Brightness.light,
      ),
      darkTheme: _buildLanAudioTheme(
        seedColor: _professionalDarkNeutralSeed,
        brightness: Brightness.dark,
        primaryColor: _darkThemeButtonBlue,
        onPrimaryColor: Colors.white,
      ),
      home: AudioHomePage(
        themeMode: _themeMode,
        onThemeModeChanged: _setThemeMode,
      ),
    );
  }
}

ThemeData _buildLanAudioTheme({
  required Color seedColor,
  required Brightness brightness,
  Color? primaryColor,
  Color? onPrimaryColor,
}) {
  final generatedColorScheme = ColorScheme.fromSeed(
    seedColor: seedColor,
    brightness: brightness,
  );
  final colorScheme = primaryColor == null
      ? generatedColorScheme
      : generatedColorScheme.copyWith(
          primary: primaryColor,
          onPrimary: onPrimaryColor,
        );

  return ThemeData(
    colorScheme: colorScheme,
    useMaterial3: true,
  );
}

ThemeMode _themeModeFromString(String? value) {
  return switch (value) {
    'light' => ThemeMode.light,
    'dark' => ThemeMode.dark,
    _ => ThemeMode.system,
  };
}

enum _PlaybackStartSource { manual, autoDiscovery }

enum _AutoStartBlockReason { manualStop, outputDeviceChanged }

class AudioHomePage extends StatefulWidget {
  const AudioHomePage({
    super.key,
    required this.themeMode,
    required this.onThemeModeChanged,
  });

  final ThemeMode themeMode;
  final ValueChanged<ThemeMode> onThemeModeChanged;

  @override
  State<AudioHomePage> createState() => _AudioHomePageState();
}

class _AudioHomePageState extends State<AudioHomePage>
    with WidgetsBindingObserver {
  static const _channel = MethodChannel(
    'dev.local.lan_audio_flutter/audio_service',
  );
  static const _bufferEnabledKey = 'autoLatencyBufferEnabled';
  static const _autoStartOnDiscoveryKey = 'autoStartOnDiscovery';
  static const _stopOnOutputDeviceChangeKey = 'stopOnOutputDeviceChange';
  static const _customBufferMsKey = 'customBufferMs';
  static const _manualHostKey = 'manualHost';
  static const _manualPortKey = 'manualPort';
  static const _minCustomBufferMs = 1;
  static const _maxBufferMs = 2000;
  static const _defaultControlPort = 9091;
  static const _foregroundScanInterval = Duration.zero;
  static const _backgroundScanInterval = Duration(seconds: 10);

  final _hostController = TextEditingController(text: '192.168.1.100');
  final _portController = TextEditingController(text: '9091');
  final _customBufferMsController = TextEditingController();

  var _isPlaying = false;
  var _isScanning = false;
  var _bufferEnabled = true;
  var _autoStartOnDiscovery = false;
  var _stopOnOutputDeviceChange = true;
  var _status = '空闲';
  var _devices = <LanAudioDevice>[];
  var _playbackStats = PlaybackStats.empty();
  String? _playbackStatsError;
  int? _lastLatencyMs;
  var _isTestingLatency = false;
  String? _latencyTestError;

  var _computerVolume = 0.0;
  var _computerMuted = false;
  var _isLoadingComputerState = false;
  var _computerControlStatus = '连接电脑后可控制系统音量';

  String? _playingHost;
  int? _playingControlPort;
  _AutoStartBlockReason? _autoStartBlockReason;
  Timer? _volumeDebounce;
  Timer? _scanRetryTimer;
  AppLifecycleState _appLifecycleState = AppLifecycleState.resumed;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    _channel.setMethodCallHandler(_handleNativeMethodCall);
    _hostController.addListener(_saveManualConnection);
    _portController.addListener(_saveManualConnection);
    _customBufferMsController.addListener(_saveCustomBufferMs);
    _loadSettings();
    WidgetsBinding.instance.addPostFrameCallback((_) => _scan());
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    _scanRetryTimer?.cancel();
    _hostController.removeListener(_saveManualConnection);
    _portController.removeListener(_saveManualConnection);
    _customBufferMsController.removeListener(_saveCustomBufferMs);
    _hostController.dispose();
    _portController.dispose();
    _customBufferMsController.dispose();
    _volumeDebounce?.cancel();
    super.dispose();
  }

  Future<void> _handleNativeMethodCall(MethodCall call) async {
    if (call.method != 'playbackStopped') return;

    final args = call.arguments;
    final reason = args is Map ? args['reason'] as String? : null;
    final host = _controlHost;
    final port = _controlPort;
    final serverStatus = reason == 'user' || host.isEmpty
        ? null
        : await _queryServerStatus(host, port);
    if (!mounted) return;

    setState(() {
      _isPlaying = false;
      _playingHost = null;
      _playingControlPort = null;
      _autoStartBlockReason = _autoStartBlockReasonForStopReason(reason);
      _status = _resolvedPlaybackStoppedStatus(reason, serverStatus);
    });
    unawaited(_refreshPlaybackStats());
    _scheduleScanRetryIfNeeded(reschedule: true);
  }

  Future<ServerStatusCheck?> _queryServerStatus(String host, int port) async {
    try {
      final result = await _channel.invokeMethod<Map<dynamic, dynamic>>(
        'queryServerStatus',
        <String, Object>{'host': host, 'port': port},
      );
      return ServerStatusCheck.fromMap(result ?? <dynamic, dynamic>{});
    } on PlatformException catch (error) {
      return ServerStatusCheck.unreachable(error.message ?? error.code);
    }
  }

  String _resolvedPlaybackStoppedStatus(
    String? reason, [
    ServerStatusCheck? serverStatus,
  ]) {
    final base = switch (reason) {
      'user' => '已手动停止，需手动重新开始',
      'udp_timeout' => '3 秒未收到 UDP 音频包，后台播放已停止',
      'output_device_changed' => '输出设备变化，已停止，需手动重新开始',
      _ => '后台播放已停止',
    };
    if (serverStatus == null) return base;
    return '$base，${serverStatus.summary}';
  }

  _AutoStartBlockReason? _autoStartBlockReasonForStopReason(String? reason) {
    return switch (reason) {
      'user' => _AutoStartBlockReason.manualStop,
      'output_device_changed' => _AutoStartBlockReason.outputDeviceChanged,
      _ => null,
    };
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    _appLifecycleState = state;
    _scheduleScanRetryIfNeeded(reschedule: true);
  }

  Future<void> _loadSettings() async {
    final prefs = await SharedPreferences.getInstance();
    final bufferEnabled = prefs.getBool(_bufferEnabledKey);
    final autoStartOnDiscovery = prefs.getBool(_autoStartOnDiscoveryKey);
    final stopOnOutputDeviceChange = prefs.getBool(
      _stopOnOutputDeviceChangeKey,
    );
    final customBufferMs = prefs.getInt(_customBufferMsKey);
    final manualHost = prefs.getString(_manualHostKey);
    final manualPort = prefs.getString(_manualPortKey);
    if (!mounted) return;

    setState(() {
      if (bufferEnabled != null) _bufferEnabled = bufferEnabled;
      if (autoStartOnDiscovery != null) {
        _autoStartOnDiscovery = autoStartOnDiscovery;
      }
      if (stopOnOutputDeviceChange != null) {
        _stopOnOutputDeviceChange = stopOnOutputDeviceChange;
      }
      if (customBufferMs != null) {
        _customBufferMsController.text = customBufferMs
            .clamp(_minCustomBufferMs, _maxBufferMs)
            .toString();
      }
      if (manualHost != null) _hostController.text = manualHost;
      if (manualPort != null) _portController.text = manualPort;
    });
  }

  Future<void> _saveBufferEnabled(bool value) async {
    final prefs = await SharedPreferences.getInstance();
    await prefs.setBool(_bufferEnabledKey, value);
  }

  Future<void> _saveAutoStartOnDiscovery(bool value) async {
    final prefs = await SharedPreferences.getInstance();
    await prefs.setBool(_autoStartOnDiscoveryKey, value);
  }

  Future<void> _saveStopOnOutputDeviceChange(bool value) async {
    final prefs = await SharedPreferences.getInstance();
    await prefs.setBool(_stopOnOutputDeviceChangeKey, value);
  }

  Future<void> _saveCustomBufferMs() async {
    final prefs = await SharedPreferences.getInstance();
    if (!_hasCustomBufferMs) {
      await prefs.remove(_customBufferMsKey);
      return;
    }
    await prefs.setInt(_customBufferMsKey, _customBufferMs);
  }

  Future<void> _saveManualConnection() async {
    final prefs = await SharedPreferences.getInstance();
    await prefs.setString(_manualHostKey, _hostController.text.trim());
    await prefs.setString(_manualPortKey, _portController.text.trim());
  }

  Future<void> _toggleManualPlayback() async {
    if (_isPlaying) {
      await _stop();
      return;
    }

    final host = _hostController.text.trim();
    final controlPort =
        int.tryParse(_portController.text.trim()) ?? _defaultControlPort;
    await _start(
      host,
      controlPort,
      source: _PlaybackStartSource.manual,
    );
  }

  Future<void> _toggleDevicePlayback(LanAudioDevice device) async {
    if (_isCurrentDevicePlaying(device)) {
      await _stop();
      return;
    }

    await _start(
      device.host,
      device.controlPort,
      frameMs: device.frameMsInt,
      source: _PlaybackStartSource.manual,
    );
  }

  Future<void> _start(
    String host,
    int controlPort, {
    int? frameMs,
    required _PlaybackStartSource source,
  }) async {
    if (source == _PlaybackStartSource.autoDiscovery &&
        _autoStartBlockReason != null) {
      return;
    }
    if (host.isEmpty) {
      setState(() => _status = '请输入 Rust 服务端 IP 地址');
      return;
    }

    setState(() {
      _status = _bufferEnabled
          ? (_hasCustomBufferMs ? '正在启动自定义缓冲播放...' : '正在检测到服务端的延迟...')
          : '正在启动无缓冲播放...';
    });

    try {
      final startArgs = <String, Object>{
        'host': host,
        'controlPort': controlPort,
        'bufferEnabled': _bufferEnabled,
        'hasCustomBufferMs': _hasCustomBufferMs,
        'stopOnOutputDeviceChange': _stopOnOutputDeviceChange,
      };
      if (_hasCustomBufferMs) startArgs['customBufferMs'] = _customBufferMs;
      if (frameMs != null) startArgs['frameMs'] = frameMs;

      final result = await _channel.invokeMethod<Map<dynamic, dynamic>>(
        'start',
        startArgs,
      );
      final bufferMs = result?['bufferMs'] as int?;
      setState(() {
        _isPlaying = true;
        _playingHost = host;
        _playingControlPort = controlPort;
        if (source == _PlaybackStartSource.manual) {
          _autoStartBlockReason = null;
        }
        _status = _bufferEnabled
            ? '后台播放已启动：$host:$controlPort，缓冲 ${bufferMs ?? '?'}ms'
            : '后台播放已启动：$host:$controlPort，缓冲已关闭';
      });
      _scanRetryTimer?.cancel();
      _scanRetryTimer = null;
      unawaited(_refreshComputerVolume());
      unawaited(_refreshPlaybackStats());
    } on PlatformException catch (error) {
      setState(() => _status = '启动失败：${error.message ?? error.code}');
      _scheduleScanRetryIfNeeded(reschedule: true);
    }
  }

  Future<void> _stop() async {
    setState(() {
      _autoStartBlockReason = _AutoStartBlockReason.manualStop;
    });
    await _channel.invokeMethod('stop');
    setState(() {
      _isPlaying = false;
      _playingHost = null;
      _playingControlPort = null;
      _status = '已手动停止，需手动重新开始';
    });
    unawaited(_refreshPlaybackStats());
    _scheduleScanRetryIfNeeded(reschedule: true);
  }

  Future<void> _scan() async {
    if (_isScanning || !mounted || _isPlaying) return;
    _scanRetryTimer?.cancel();
    _scanRetryTimer = null;

    setState(() => _isScanning = true);

    try {
      final result = await _channel.invokeMethod<List<dynamic>>('scanMdns');
      final devices =
          (result ?? [])
              .whereType<Map<dynamic, dynamic>>()
              .map(LanAudioDevice.fromMap)
              .where((device) => device.host.isNotEmpty)
              .toList()
            ..sort((a, b) => a.name.compareTo(b.name));

      setState(() {
        _devices = devices;
      });
      if (_canAutoStartDiscoveredDevice && devices.isNotEmpty && !_isPlaying) {
        await _startDiscoveredDevice(devices.first);
      }
    } on PlatformException catch (error) {
      debugPrint('扫描失败：${error.message ?? error.code}');
    } finally {
      if (mounted) {
        setState(() => _isScanning = false);
        _scheduleScanRetryIfNeeded();
      }
    }
  }

  Future<void> _startDiscoveredDevice(LanAudioDevice device) async {
    if (!_canAutoStartDiscoveredDevice) return;
    await _start(
      device.host,
      device.controlPort,
      frameMs: device.frameMsInt,
      source: _PlaybackStartSource.autoDiscovery,
    );
  }

  void _scheduleScanRetryIfNeeded({bool reschedule = false}) {
    if (!mounted || _isPlaying) {
      _scanRetryTimer?.cancel();
      _scanRetryTimer = null;
      return;
    }
    if (_isScanning) return;
    if (_scanRetryTimer != null && !reschedule) return;

    _scanRetryTimer?.cancel();
    _scanRetryTimer = Timer(_scanInterval, _scan);
  }

  Duration get _scanInterval => _appLifecycleState == AppLifecycleState.resumed
      ? _foregroundScanInterval
      : _backgroundScanInterval;

  Future<void> _refreshPlaybackStats() async {
    try {
      final stats = await _channel.invokeMethod<Map<dynamic, dynamic>>(
        'getPlaybackStats',
      );
      if (!mounted) return;
      setState(() {
        _playbackStats = PlaybackStats.fromMap(stats ?? <dynamic, dynamic>{});
        _playbackStatsError = null;
      });
    } on MissingPluginException catch (error) {
      if (!mounted) return;
      setState(() {
        _playbackStatsError = error.message ?? '当前平台不支持播放统计';
      });
    } on PlatformException catch (error) {
      if (!mounted) return;
      setState(() {
        _playbackStatsError = error.message ?? error.code;
      });
    }
  }

  Future<void> _clearPlaybackStats() async {
    try {
      final stats = await _channel.invokeMethod<Map<dynamic, dynamic>>(
        'clearPlaybackStats',
      );
      if (!mounted) return;
      setState(() {
        _playbackStats = PlaybackStats.fromMap(stats ?? <dynamic, dynamic>{});
        _playbackStatsError = null;
        _lastLatencyMs = null;
        _latencyTestError = null;
      });
    } on PlatformException catch (error) {
      if (!mounted) return;
      setState(() {
        _playbackStatsError = error.message ?? error.code;
      });
    }
  }

  Future<void> _testLatency() async {
    final host = _controlHost;
    if (host.isEmpty) {
      setState(() {
        _latencyTestError = '请先输入或连接电脑 IP';
      });
      return;
    }

    setState(() {
      _isTestingLatency = true;
      _latencyTestError = null;
    });

    try {
      final result = await _channel.invokeMethod<Map<dynamic, dynamic>>(
        'measureLatency',
        <String, Object>{'host': host, 'port': _controlPort},
      );
      if (!mounted) return;
      setState(() {
        _lastLatencyMs = PlaybackStats.readInt(result?['latencyMs']);
      });
    } on PlatformException catch (error) {
      if (!mounted) return;
      setState(() {
        _latencyTestError = error.message ?? error.code;
      });
    } finally {
      if (mounted) {
        setState(() {
          _isTestingLatency = false;
        });
      }
    }
  }

  Future<void> _refreshComputerVolume() async {
    final host = _controlHost;
    if (host.isEmpty) {
      setState(() => _computerControlStatus = '请先输入或连接电脑 IP');
      return;
    }

    setState(() {
      _isLoadingComputerState = true;
      _computerControlStatus = '正在读取电脑音量...';
    });

    try {
      final state = await _sendComputerControl('get');
      if (!mounted) return;
      setState(() {
        _applyComputerState(state);
        _computerControlStatus = '已同步电脑音量';
      });
    } on PlatformException catch (error) {
      if (!mounted) return;
      setState(() {
        _computerControlStatus = '读取失败：${error.message ?? error.code}';
      });
    } finally {
      if (mounted) setState(() => _isLoadingComputerState = false);
    }
  }

  void _setComputerVolume(double value) {
    setState(() {
      _computerVolume = value;
      if (value > 0 && _computerMuted) _computerMuted = false;
    });

    _volumeDebounce?.cancel();
    _volumeDebounce = Timer(const Duration(milliseconds: 160), () {
      unawaited(_sendAndApplyComputerControl('setVolume', value.round()));
    });
  }

  Future<void> _setComputerMuted(bool muted) async {
    _volumeDebounce?.cancel();
    setState(() => _computerMuted = muted);
    await _sendAndApplyComputerControl('setMute', muted ? 1 : 0);
  }

  Future<void> _sendAndApplyComputerControl(
    String command, [
    int? value,
  ]) async {
    try {
      final state = await _sendComputerControl(command, value);
      if (!mounted) return;
      setState(() {
        _applyComputerState(state);
        _computerControlStatus = '电脑控制已发送';
      });
    } on PlatformException catch (error) {
      if (!mounted) return;
      setState(() {
        _computerControlStatus = '控制失败：${error.message ?? error.code}';
      });
    }
  }

  Future<Map<dynamic, dynamic>> _sendComputerControl(
    String command, [
    int? value,
  ]) async {
    final host = _controlHost;
    if (host.isEmpty) {
      throw PlatformException(code: 'invalid_host', message: 'Host is empty');
    }

    final args = <String, Object>{
      'host': host,
      'port': _controlPort,
      'command': command,
    };
    if (value != null) args['value'] = value;

    return await _channel.invokeMethod<Map<dynamic, dynamic>>(
          'computerControl',
          args,
        ) ??
        <dynamic, dynamic>{};
  }

  void _applyComputerState(Map<dynamic, dynamic> state) {
    final volume = state['volume'];
    final muted = state['muted'];
    if (volume is int) _computerVolume = volume.clamp(0, 100).toDouble();
    if (muted is bool) _computerMuted = muted;
  }

  bool _isCurrentDevicePlaying(LanAudioDevice device) {
    return _isPlaying &&
        _playingHost == device.host &&
        _playingControlPort == device.controlPort;
  }

  String get _controlHost => _playingHost ?? _hostController.text.trim();

  int get _controlPort =>
      _playingControlPort ??
      int.tryParse(_portController.text.trim()) ??
      _defaultControlPort;

  int get _customBufferMs {
    final raw = int.tryParse(_customBufferMsController.text.trim()) ?? 0;
    return raw.clamp(_minCustomBufferMs, _maxBufferMs);
  }

  bool get _hasCustomBufferMs =>
      _customBufferMsController.text.trim().isNotEmpty;

  bool get _canAutoStartDiscoveredDevice =>
      _autoStartOnDiscovery && _autoStartBlockReason == null;

  @override
  Widget build(BuildContext context) {
    return DefaultTabController(
      length: 5,
      child: Builder(
        builder: (context) {
          final tabController = DefaultTabController.of(context);

          return Scaffold(
            appBar: AppBar(title: const Text('Audio Link')),
            body: SafeArea(
              child: TabBarView(
                controller: tabController,
                children: [
                  _ScanTab(
                    devices: _devices,
                    isScanning: _isScanning,
                    isDevicePlaying: _isCurrentDevicePlaying,
                    onTogglePlayback: _toggleDevicePlayback,
                  ),
                  _ManualTab(
                    hostController: _hostController,
                    portController: _portController,
                    isPlaying: _isPlaying,
                    status: _status,
                    onTogglePlayback: _toggleManualPlayback,
                  ),
                  _ControlTab(
                    computerVolume: _computerVolume,
                    computerMuted: _computerMuted,
                    isLoadingComputerState: _isLoadingComputerState,
                    computerControlStatus: _computerControlStatus,
                    onVisible: _refreshComputerVolume,
                    onComputerVolumeChanged: _setComputerVolume,
                    onComputerMuteChanged: _setComputerMuted,
                    onComputerPrevious: () =>
                        _sendAndApplyComputerControl('previous'),
                    onComputerPlayPause: () =>
                        _sendAndApplyComputerControl('playPause'),
                    onComputerNext: () => _sendAndApplyComputerControl('next'),
                    onRefreshComputerState: _refreshComputerVolume,
                  ),
                  _LogTab(
                    stats: _playbackStats,
                    error: _playbackStatsError,
                    lastLatencyMs: _lastLatencyMs,
                    isTestingLatency: _isTestingLatency,
                    latencyTestError: _latencyTestError,
                    onVisible: _refreshPlaybackStats,
                    onRefresh: _refreshPlaybackStats,
                    onClear: _clearPlaybackStats,
                    onTestLatency: _testLatency,
                  ),
                  _SettingsTab(
                    themeMode: widget.themeMode,
                    autoStartOnDiscovery: _autoStartOnDiscovery,
                    stopOnOutputDeviceChange: _stopOnOutputDeviceChange,
                    bufferEnabled: _bufferEnabled,
                    customBufferMsController: _customBufferMsController,
                    maxBufferMs: _maxBufferMs,
                    onThemeModeChanged: widget.onThemeModeChanged,
                    onAutoStartOnDiscoveryChanged: (value) {
                      _saveAutoStartOnDiscovery(value);
                      setState(() {
                        _autoStartOnDiscovery = value;
                      });
                    },
                    onStopOnOutputDeviceChangeChanged: (value) {
                      _saveStopOnOutputDeviceChange(value);
                      setState(() {
                        _stopOnOutputDeviceChange = value;
                      });
                    },
                    onBufferEnabledChanged: (value) {
                      _saveBufferEnabled(value);
                      setState(() {
                        _bufferEnabled = value;
                        if (_isPlaying) {
                          _status = value ? '缓冲将在下次开始播放时生效' : '缓冲将在下次开始播放时关闭';
                        }
                      });
                    },
                    onCustomBufferSubmitted: (_) {
                      if (!_hasCustomBufferMs) {
                        _saveCustomBufferMs();
                        return;
                      }
                      final clamped = _customBufferMs;
                      _customBufferMsController.text = clamped.toString();
                      _customBufferMsController.selection =
                          TextSelection.collapsed(
                            offset: _customBufferMsController.text.length,
                          );
                      _saveCustomBufferMs();
                    },
                    onCustomBufferCleared: () {
                      _customBufferMsController.clear();
                      _saveCustomBufferMs();
                    },
                  ),
                ],
              ),
            ),
            bottomNavigationBar: AnimatedBuilder(
              animation: tabController,
              builder: (context, _) {
                return NavigationBar(
                  elevation: 0,
                  selectedIndex: tabController.index,
                  onDestinationSelected: (index) {
                    if (index == tabController.index) return;
                    tabController.animateTo(index);
                  },
                  destinations: const [
                    NavigationDestination(icon: Icon(Icons.radar), label: '扫描'),
                    NavigationDestination(icon: Icon(Icons.edit), label: '手动'),
                    NavigationDestination(
                      icon: Icon(Icons.gamepad),
                      label: '控制',
                    ),
                    NavigationDestination(
                      icon: Icon(Icons.receipt_long),
                      label: '日志',
                    ),
                    NavigationDestination(icon: Icon(Icons.tune), label: '设置'),
                  ],
                );
              },
            ),
          );
        },
      ),
    );
  }
}

class _ControlTab extends StatefulWidget {
  const _ControlTab({
    required this.computerVolume,
    required this.computerMuted,
    required this.isLoadingComputerState,
    required this.computerControlStatus,
    required this.onVisible,
    required this.onComputerVolumeChanged,
    required this.onComputerMuteChanged,
    required this.onComputerPrevious,
    required this.onComputerPlayPause,
    required this.onComputerNext,
    required this.onRefreshComputerState,
  });

  final double computerVolume;
  final bool computerMuted;
  final bool isLoadingComputerState;
  final String computerControlStatus;
  final VoidCallback onVisible;
  final ValueChanged<double> onComputerVolumeChanged;
  final ValueChanged<bool> onComputerMuteChanged;
  final VoidCallback onComputerPrevious;
  final VoidCallback onComputerPlayPause;
  final VoidCallback onComputerNext;
  final VoidCallback onRefreshComputerState;

  @override
  State<_ControlTab> createState() => _ControlTabState();
}

class _ControlTabState extends State<_ControlTab> {
  TabController? _tabController;

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final controller = DefaultTabController.maybeOf(context);
    if (_tabController == controller) return;
    _tabController?.removeListener(_handleTabChanged);
    _tabController = controller;
    _tabController?.addListener(_handleTabChanged);
    _notifyIfVisible();
  }

  @override
  void dispose() {
    _tabController?.removeListener(_handleTabChanged);
    super.dispose();
  }

  void _handleTabChanged() {
    if (_tabController?.indexIsChanging == true) return;
    _notifyIfVisible();
  }

  void _notifyIfVisible() {
    if (_tabController?.index == 2) {
      WidgetsBinding.instance.addPostFrameCallback((_) => widget.onVisible());
    }
  }

  @override
  Widget build(BuildContext context) {
    return ListView(
      padding: const EdgeInsets.all(20),
      children: [
        Text('电脑控制', style: Theme.of(context).textTheme.headlineSmall),
        const SizedBox(height: 12),
        Card(
          child: Padding(
            padding: const EdgeInsets.all(16),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Row(
                  children: [
                    const Icon(Icons.volume_up),
                    const SizedBox(width: 12),
                    Expanded(
                      child: Text(
                        '电脑音量 ${widget.computerVolume.round()}%',
                        style: Theme.of(context).textTheme.titleMedium,
                      ),
                    ),
                    IconButton(
                      onPressed: widget.isLoadingComputerState
                          ? null
                          : widget.onRefreshComputerState,
                      icon: widget.isLoadingComputerState
                          ? const SizedBox.square(
                              dimension: 18,
                              child: CircularProgressIndicator(strokeWidth: 2),
                            )
                          : const Icon(Icons.refresh),
                      tooltip: '刷新音量',
                    ),
                  ],
                ),
                Slider(
                  value: widget.computerVolume.clamp(0, 100),
                  min: 0,
                  max: 100,
                  divisions: 100,
                  label: '${widget.computerVolume.round()}%',
                  onChanged: widget.onComputerVolumeChanged,
                ),
                SwitchListTile(
                  contentPadding: EdgeInsets.zero,
                  secondary: Icon(
                    widget.computerMuted ? Icons.volume_off : Icons.volume_up,
                  ),
                  title: const Text('静音'),
                  value: widget.computerMuted,
                  onChanged: widget.onComputerMuteChanged,
                ),
                const SizedBox(height: 8),
                Row(
                  children: [
                    Expanded(
                      child: IconButton(
                        onPressed: widget.onComputerPrevious,
                        iconSize: 30,
                        icon: const Icon(Icons.skip_previous),
                        tooltip: '上一首',
                      ),
                    ),
                    const SizedBox(width: 12),
                    Expanded(
                      child: IconButton(
                        onPressed: widget.onComputerPlayPause,
                        iconSize: 30,
                        icon: const Icon(Icons.pause),
                        tooltip: '暂停',
                      ),
                    ),
                    const SizedBox(width: 12),
                    Expanded(
                      child: IconButton(
                        onPressed: widget.onComputerNext,
                        iconSize: 30,
                        icon: const Icon(Icons.skip_next),
                        tooltip: '下一首',
                      ),
                    ),
                  ],
                ),
                const SizedBox(height: 12),
                Text(widget.computerControlStatus),
              ],
            ),
          ),
        ),
      ],
    );
  }
}

class _LogTab extends StatefulWidget {
  const _LogTab({
    required this.stats,
    required this.error,
    required this.lastLatencyMs,
    required this.isTestingLatency,
    required this.latencyTestError,
    required this.onVisible,
    required this.onRefresh,
    required this.onClear,
    required this.onTestLatency,
  });

  final PlaybackStats stats;
  final String? error;
  final int? lastLatencyMs;
  final bool isTestingLatency;
  final String? latencyTestError;
  final VoidCallback onVisible;
  final Future<void> Function() onRefresh;
  final Future<void> Function() onClear;
  final VoidCallback onTestLatency;

  @override
  State<_LogTab> createState() => _LogTabState();
}

class _LogTabState extends State<_LogTab> {
  static const _tabIndex = 3;
  static const _refreshInterval = Duration(seconds: 1);

  TabController? _tabController;
  Timer? _refreshTimer;

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final controller = DefaultTabController.maybeOf(context);
    if (_tabController == controller) {
      _syncRefreshTimer();
      return;
    }
    _tabController?.removeListener(_handleTabChanged);
    _tabController = controller;
    _tabController?.addListener(_handleTabChanged);
    _syncRefreshTimer();
  }

  @override
  void dispose() {
    _refreshTimer?.cancel();
    _tabController?.removeListener(_handleTabChanged);
    super.dispose();
  }

  void _handleTabChanged() {
    if (_tabController?.indexIsChanging == true) return;
    _syncRefreshTimer();
  }

  void _syncRefreshTimer() {
    final visible = _tabController?.index == _tabIndex;
    if (!visible) {
      _refreshTimer?.cancel();
      _refreshTimer = null;
      return;
    }

    WidgetsBinding.instance.addPostFrameCallback((_) => widget.onVisible());
    _refreshTimer ??= Timer.periodic(
      _refreshInterval,
      (_) => widget.onVisible(),
    );
  }

  @override
  Widget build(BuildContext context) {
    final stats = widget.stats;
    final theme = Theme.of(context);
    final connection = stats.host.isEmpty
        ? '未连接'
        : '${stats.host}:${stats.controlPort}';

    return RefreshIndicator(
      onRefresh: widget.onRefresh,
      child: ListView(
        padding: const EdgeInsets.all(20),
        children: [
          Row(
            children: [
              Expanded(child: Text('日志', style: theme.textTheme.headlineSmall)),
              IconButton(
                onPressed: widget.onRefresh,
                icon: const Icon(Icons.refresh),
                tooltip: '刷新统计',
              ),
              IconButton(
                onPressed: widget.onClear,
                icon: const Icon(Icons.delete_sweep),
                tooltip: '清空日志',
              ),
            ],
          ),
          const SizedBox(height: 12),
          Card(
            child: Padding(
              padding: const EdgeInsets.all(16),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  _LogMetric(
                    icon: Icons.warning_amber,
                    label: '丢包次数',
                    value: stats.lostPackets.toString(),
                    emphasized: true,
                  ),
                  const Divider(height: 28),
                  _LogMetric(
                    icon: Icons.call_received,
                    label: '收到包数',
                    value: stats.receivedPackets.toString(),
                  ),
                  _LogMetric(
                    icon: Icons.timer_off,
                    label: '超时丢弃包数',
                    value: stats.timedOutPackets.toString(),
                  ),
                  _LogMetric(
                    icon: Icons.block,
                    label: '丢弃包数',
                    value: stats.discardedPackets.toString(),
                  ),
                  _LogMetric(
                    icon: Icons.graphic_eq,
                    label: '播放缓冲耗空次数',
                    value: stats.audioTrackUnderruns.toString(),
                  ),
                  _LogMetric(
                    icon: Icons.av_timer,
                    label: '平均收包间隔',
                    value: '${stats.averageReceiveIntervalMs} ms',
                  ),
                  _LogMetric(
                    icon: Icons.swap_vert,
                    label: '最大收包间隔',
                    value: '${stats.maxReceiveIntervalMs} ms',
                  ),
                  _LogMetric(
                    icon: Icons.inventory_2,
                    label: '当前缓冲',
                    value: stats.bufferSummary,
                  ),
                  _LogMetric(
                    icon: Icons.flag,
                    label: '目标缓冲',
                    value: stats.targetBufferSummary,
                  ),
                  _LogMetric(
                    icon: Icons.format_list_numbered,
                    label: '有序缓冲',
                    value: stats.orderedBufferSummary,
                  ),
                  _LogMetric(
                    icon: Icons.open_in_full,
                    label: '播放倍率',
                    value: '${stats.playbackStretchRatio.toStringAsFixed(3)}x',
                  ),
                ],
              ),
            ),
          ),
          const SizedBox(height: 12),
          Card(
            child: Padding(
              padding: const EdgeInsets.all(16),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  FilledButton.icon(
                    onPressed: widget.isTestingLatency
                        ? null
                        : widget.onTestLatency,
                    icon: widget.isTestingLatency
                        ? const SizedBox.square(
                            dimension: 18,
                            child: CircularProgressIndicator(strokeWidth: 2),
                          )
                        : const Icon(Icons.network_ping),
                    label: Text(
                      widget.isTestingLatency ? '正在测试 UDP 延迟...' : '测试 UDP 延迟',
                    ),
                  ),
                  const SizedBox(height: 12),
                  _LogMetric(
                    icon: Icons.speed,
                    label: '最近 UDP 延迟',
                    value: widget.lastLatencyMs == null
                        ? '-'
                        : '${widget.lastLatencyMs} ms',
                  ),
                  if (widget.latencyTestError != null) ...[
                    const SizedBox(height: 8),
                    Text(
                      'UDP 延迟测试失败：${widget.latencyTestError}',
                      style: TextStyle(color: theme.colorScheme.error),
                    ),
                  ],
                ],
              ),
            ),
          ),
          const SizedBox(height: 12),
          Card(
            child: Padding(
              padding: const EdgeInsets.all(16),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  _LogMetric(
                    icon: stats.isRunning ? Icons.play_circle : Icons.pause,
                    label: '播放状态',
                    value: stats.isRunning ? '播放中' : '未播放',
                  ),
                  _LogMetric(icon: Icons.link, label: '连接', value: connection),
                  _LogMetric(
                    icon: Icons.timer,
                    label: '持续时间',
                    value: stats.startedAtMs == 0
                        ? '-'
                        : _formatDuration(stats.runningDuration),
                  ),
                ],
              ),
            ),
          ),
          if (widget.error != null) ...[
            const SizedBox(height: 12),
            Text(
              '统计读取失败：${widget.error}',
              style: TextStyle(color: theme.colorScheme.error),
            ),
          ],
        ],
      ),
    );
  }

  String _formatDuration(Duration duration) {
    final hours = duration.inHours;
    final minutes = duration.inMinutes.remainder(60).toString().padLeft(2, '0');
    final seconds = duration.inSeconds.remainder(60).toString().padLeft(2, '0');
    if (hours > 0) return '$hours:$minutes:$seconds';
    return '$minutes:$seconds';
  }
}

class _LogMetric extends StatelessWidget {
  const _LogMetric({
    required this.icon,
    required this.label,
    required this.value,
    this.emphasized = false,
  });

  final IconData icon;
  final String label;
  final String value;
  final bool emphasized;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final valueStyle = emphasized
        ? theme.textTheme.headlineMedium?.copyWith(
            color: theme.colorScheme.error,
            fontWeight: FontWeight.w700,
          )
        : theme.textTheme.titleMedium;

    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 6),
      child: Row(
        children: [
          Icon(icon, size: 22),
          const SizedBox(width: 12),
          Expanded(child: Text(label)),
          Text(value, style: valueStyle),
        ],
      ),
    );
  }
}

class _SettingsTab extends StatelessWidget {
  const _SettingsTab({
    required this.themeMode,
    required this.autoStartOnDiscovery,
    required this.stopOnOutputDeviceChange,
    required this.bufferEnabled,
    required this.customBufferMsController,
    required this.maxBufferMs,
    required this.onThemeModeChanged,
    required this.onAutoStartOnDiscoveryChanged,
    required this.onStopOnOutputDeviceChangeChanged,
    required this.onBufferEnabledChanged,
    required this.onCustomBufferSubmitted,
    required this.onCustomBufferCleared,
  });

  final ThemeMode themeMode;
  final bool autoStartOnDiscovery;
  final bool stopOnOutputDeviceChange;
  final bool bufferEnabled;
  final TextEditingController customBufferMsController;
  final int maxBufferMs;
  final ValueChanged<ThemeMode> onThemeModeChanged;
  final ValueChanged<bool> onAutoStartOnDiscoveryChanged;
  final ValueChanged<bool> onStopOnOutputDeviceChangeChanged;
  final ValueChanged<bool> onBufferEnabledChanged;
  final ValueChanged<String> onCustomBufferSubmitted;
  final VoidCallback onCustomBufferCleared;

  @override
  Widget build(BuildContext context) {
    return ListView(
      padding: const EdgeInsets.all(20),
      children: [
        Text('播放设置', style: Theme.of(context).textTheme.headlineSmall),
        const SizedBox(height: 12),
        Card(
          child: Padding(
            padding: const EdgeInsets.all(16),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Text('主题', style: Theme.of(context).textTheme.titleMedium),
                const SizedBox(height: 12),
                SegmentedButton<ThemeMode>(
                  segments: const [
                    ButtonSegment(
                      value: ThemeMode.system,
                      icon: Icon(Icons.brightness_auto),
                      label: Text('跟随'),
                    ),
                    ButtonSegment(
                      value: ThemeMode.light,
                      icon: Icon(Icons.light_mode),
                      label: Text('浅色'),
                    ),
                    ButtonSegment(
                      value: ThemeMode.dark,
                      icon: Icon(Icons.dark_mode),
                      label: Text('深色'),
                    ),
                  ],
                  selected: {themeMode},
                  onSelectionChanged: (selection) {
                    onThemeModeChanged(selection.first);
                  },
                ),
              ],
            ),
          ),
        ),
        const SizedBox(height: 12),
        Card(
          child: Padding(
            padding: const EdgeInsets.all(16),
            child: Column(
              children: [
                SwitchListTile(
                  contentPadding: EdgeInsets.zero,
                  secondary: const Icon(Icons.play_circle),
                  title: const Text('发现后自动串流'),
                  subtitle: const Text('扫描到服务端后自动开始播放第一个发现的设备'),
                  value: autoStartOnDiscovery,
                  onChanged: onAutoStartOnDiscoveryChanged,
                ),
                SwitchListTile(
                  contentPadding: EdgeInsets.zero,
                  secondary: const Icon(Icons.speaker),
                  title: const Text('输出设备变化后停止串流'),
                  subtitle: const Text('播放期间耳机、蓝牙或扬声器输出变化时自动关闭后台串流'),
                  value: stopOnOutputDeviceChange,
                  onChanged: onStopOnOutputDeviceChangeChanged,
                ),
                const Divider(height: 28),
                SwitchListTile(
                  contentPadding: EdgeInsets.zero,
                  secondary: const Icon(Icons.speed),
                  title: const Text('开启缓冲'),
                  subtitle: Text(
                    bufferEnabled ? '默认自动检测，填写自定义值后优先使用' : '缓冲 0ms，已关闭',
                  ),
                  value: bufferEnabled,
                  onChanged: onBufferEnabledChanged,
                ),
                const SizedBox(height: 12),
                TextField(
                  controller: customBufferMsController,
                  enabled: bufferEnabled,
                  keyboardType: TextInputType.number,
                  inputFormatters: [FilteringTextInputFormatter.digitsOnly],
                  decoration: InputDecoration(
                    labelText: '自定义缓冲',
                    suffixText: 'ms',
                    suffixIcon: IconButton(
                      onPressed: bufferEnabled ? onCustomBufferCleared : null,
                      icon: const Icon(Icons.clear),
                      tooltip: '清空自定义缓冲',
                    ),
                    helperText: '留空则自动检测，范围 1-$maxBufferMs ms',
                    border: const OutlineInputBorder(),
                  ),
                  onSubmitted: onCustomBufferSubmitted,
                  onEditingComplete: () {
                    onCustomBufferSubmitted(customBufferMsController.text);
                  },
                ),
              ],
            ),
          ),
        ),
      ],
    );
  }
}

class _ScanTab extends StatelessWidget {
  const _ScanTab({
    required this.devices,
    required this.isScanning,
    required this.isDevicePlaying,
    required this.onTogglePlayback,
  });

  final List<LanAudioDevice> devices;
  final bool isScanning;
  final bool Function(LanAudioDevice device) isDevicePlaying;
  final ValueChanged<LanAudioDevice> onTogglePlayback;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);

    if (devices.isEmpty) {
      return Padding(
        padding: const EdgeInsets.all(20),
        child: Stack(
          children: [
            Align(
              alignment: Alignment.topLeft,
              child: Text('服务端', style: theme.textTheme.headlineSmall),
            ),
            Center(
              child: ConstrainedBox(
                constraints: const BoxConstraints(maxWidth: 420),
                child: isScanning
                    ? const _ScanProgress(
                        key: ValueKey('scan-progress-centered'),
                      )
                    : const _ScanEmptyState(isScanning: false),
              ),
            ),
          ],
        ),
      );
    }

    return ListView(
      padding: const EdgeInsets.all(20),
      children: [
        Text('服务端', style: theme.textTheme.headlineSmall),
        const SizedBox(height: 8),
        for (final device in devices)
          _DeviceTile(
            device: device,
            isPlaying: isDevicePlaying(device),
            onTogglePlayback: () => onTogglePlayback(device),
          ),
      ],
    );
  }
}

class _ScanProgress extends StatelessWidget {
  const _ScanProgress({super.key});

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);

    return Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        ClipRRect(
          borderRadius: BorderRadius.circular(4),
          child: const LinearProgressIndicator(minHeight: 6),
        ),
        const SizedBox(height: 8),
        Text(
          '正在扫描局域网服务...',
          style: theme.textTheme.bodyMedium?.copyWith(
            color: theme.colorScheme.onSurfaceVariant,
          ),
        ),
      ],
    );
  }
}

class _ScanEmptyState extends StatelessWidget {
  const _ScanEmptyState({required this.isScanning});

  final bool isScanning;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);

    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 24),
      child: Text(
        isScanning ? '正在查找可用服务端' : '暂无可用服务端',
        textAlign: TextAlign.center,
        style: theme.textTheme.bodyLarge?.copyWith(
          color: theme.colorScheme.onSurfaceVariant,
        ),
      ),
    );
  }
}

class _DeviceTile extends StatelessWidget {
  const _DeviceTile({
    required this.device,
    required this.isPlaying,
    required this.onTogglePlayback,
  });

  final LanAudioDevice device;
  final bool isPlaying;
  final VoidCallback onTogglePlayback;

  @override
  Widget build(BuildContext context) {
    return Card(
      child: ListTile(
        title: Text(device.name),
        subtitle: Text.rich(
          TextSpan(
            children: [
              TextSpan(
                text: '${device.host}:${device.controlPort}  ',
                style: Theme.of(context).textTheme.bodyLarge?.copyWith(
                  color: Theme.of(context).colorScheme.onSurfaceVariant,
                  fontSize: 16,
                ),
              ),
              TextSpan(
                text: device.latencyLabel,
                style: Theme.of(context).textTheme.bodyLarge?.copyWith(
                  fontSize: 16,
                ),
              ),
            ],
          ),
        ),
        trailing: IconButton.filled(
          onPressed: onTogglePlayback,
          icon: Icon(
            isPlaying ? Icons.stop : Icons.play_arrow,
            color: Colors.white,
          ),
          tooltip: isPlaying ? '停止' : '播放',
        ),
      ),
    );
  }
}

class _ManualTab extends StatelessWidget {
  const _ManualTab({
    required this.hostController,
    required this.portController,
    required this.isPlaying,
    required this.status,
    required this.onTogglePlayback,
  });

  final TextEditingController hostController;
  final TextEditingController portController;
  final bool isPlaying;
  final String status;
  final VoidCallback onTogglePlayback;

  @override
  Widget build(BuildContext context) {
    return ListView(
      padding: const EdgeInsets.all(20),
      children: [
        Text('手动连接', style: Theme.of(context).textTheme.headlineSmall),
        const SizedBox(height: 8),
        const Text('当路由器、防火墙或手机系统限制 mDNS 发现时，可以直接输入电脑端 IP 和控制端口。'),
        const SizedBox(height: 24),
        TextField(
          controller: hostController,
          keyboardType: TextInputType.url,
          decoration: const InputDecoration(
            labelText: '服务端 IP',
            border: OutlineInputBorder(),
          ),
        ),
        const SizedBox(height: 12),
        TextField(
          controller: portController,
          keyboardType: TextInputType.number,
          decoration: const InputDecoration(
            labelText: 'TCP 控制端口',
            border: OutlineInputBorder(),
          ),
        ),
        const SizedBox(height: 20),
        FilledButton.icon(
          onPressed: onTogglePlayback,
          icon: Icon(isPlaying ? Icons.stop : Icons.play_arrow),
          label: Text(isPlaying ? '停止后台播放' : '开始后台播放'),
        ),
        const SizedBox(height: 16),
        Text(status),
      ],
    );
  }
}

class LanAudioDevice {
  const LanAudioDevice({
    required this.name,
    required this.host,
    required this.controlPort,
    this.sampleRate,
    this.channels,
    this.frameMs,
    this.format,
    this.latencyMs,
  });

  final String name;
  final String host;
  final int controlPort;
  final String? sampleRate;
  final String? channels;
  final String? frameMs;
  final String? format;
  final int? latencyMs;

  int? get frameMsInt => int.tryParse(frameMs ?? '');
  String get latencyLabel => latencyMs == null ? '' : '$latencyMs ms';

  factory LanAudioDevice.fromMap(Map<dynamic, dynamic> map) {
    return LanAudioDevice(
      name: (map['name'] as String?)?.trim().isNotEmpty == true
          ? map['name'] as String
          : 'Audio Link',
      host: map['host'] as String? ?? '',
      controlPort:
          (map['controlPort'] as int?) ?? (map['port'] as int?) ?? 9091,
      sampleRate: map['sampleRate'] as String?,
      channels: map['channels'] as String?,
      frameMs: map['frameMs'] as String?,
      format: map['format'] as String?,
      latencyMs: PlaybackStats.readIntOrNull(map['latencyMs']),
    );
  }
}

class ServerStatusCheck {
  const ServerStatusCheck({
    required this.reachable,
    required this.running,
    required this.udpSession,
    required this.audioActive,
    required this.targetAddr,
    required this.error,
  });

  final bool reachable;
  final bool running;
  final bool udpSession;
  final bool audioActive;
  final String targetAddr;
  final String? error;

  factory ServerStatusCheck.fromMap(Map<dynamic, dynamic> map) {
    return ServerStatusCheck(
      reachable: map['reachable'] == true,
      running: map['running'] == true,
      udpSession: map['udpSession'] == true,
      audioActive: map['audioActive'] == true,
      targetAddr: map['targetAddr'] as String? ?? '',
      error: null,
    );
  }

  factory ServerStatusCheck.unreachable(String error) {
    return ServerStatusCheck(
      reachable: false,
      running: false,
      udpSession: false,
      audioActive: false,
      targetAddr: '',
      error: error,
    );
  }

  String get summary {
    if (!reachable) return '服务端状态查询失败：${error ?? '不可达'}';
    if (!running) return '服务端未正常运行';
    if (udpSession || audioActive) {
      final target = targetAddr.isEmpty ? '' : '，目标 $targetAddr';
      return '服务端状态正常，仍有 UDP 会话$target';
    }
    return '服务端可达，但当前没有 UDP 音频会话';
  }
}

class PlaybackStats {
  const PlaybackStats({
    required this.isRunning,
    required this.host,
    required this.controlPort,
    required this.startedAtMs,
    required this.receivedPackets,
    required this.lostPackets,
    required this.timedOutPackets,
    required this.discardedPackets,
    required this.audioTrackUnderruns,
    required this.averageReceiveIntervalMs,
    required this.maxReceiveIntervalMs,
    required this.frameMs,
    required this.bufferedFrames,
    required this.orderedBufferedFrames,
    required this.targetBufferFrames,
    required this.bufferedMs,
    required this.orderedBufferedMs,
    required this.targetBufferMs,
    required this.playbackStretchRatio,
  });

  final bool isRunning;
  final String host;
  final int controlPort;
  final int startedAtMs;
  final int receivedPackets;
  final int lostPackets;
  final int timedOutPackets;
  final int discardedPackets;
  final int audioTrackUnderruns;
  final int averageReceiveIntervalMs;
  final int maxReceiveIntervalMs;
  final int frameMs;
  final int bufferedFrames;
  final int orderedBufferedFrames;
  final int targetBufferFrames;
  final int bufferedMs;
  final int orderedBufferedMs;
  final int targetBufferMs;
  final double playbackStretchRatio;

  factory PlaybackStats.empty() {
    return const PlaybackStats(
      isRunning: false,
      host: '',
      controlPort: 0,
      startedAtMs: 0,
      receivedPackets: 0,
      lostPackets: 0,
      timedOutPackets: 0,
      discardedPackets: 0,
      audioTrackUnderruns: 0,
      averageReceiveIntervalMs: 0,
      maxReceiveIntervalMs: 0,
      frameMs: 0,
      bufferedFrames: 0,
      orderedBufferedFrames: 0,
      targetBufferFrames: 0,
      bufferedMs: 0,
      orderedBufferedMs: 0,
      targetBufferMs: 0,
      playbackStretchRatio: 1,
    );
  }

  factory PlaybackStats.fromMap(Map<dynamic, dynamic> map) {
    return PlaybackStats(
      isRunning: map['isRunning'] == true,
      host: map['host'] as String? ?? '',
      controlPort: readInt(map['controlPort']),
      startedAtMs: readInt(map['startedAtMs']),
      receivedPackets: readInt(map['receivedPackets']),
      lostPackets: readInt(map['lostPackets']),
      timedOutPackets: readInt(map['timedOutPackets']),
      discardedPackets: readInt(map['discardedPackets']),
      audioTrackUnderruns: readInt(map['audioTrackUnderruns']),
      averageReceiveIntervalMs: readInt(map['averageReceiveIntervalMs']),
      maxReceiveIntervalMs: readInt(map['maxReceiveIntervalMs']),
      frameMs: readInt(map['frameMs']),
      bufferedFrames: readInt(map['bufferedFrames']),
      orderedBufferedFrames: readInt(map['orderedBufferedFrames']),
      targetBufferFrames: readInt(map['targetBufferFrames']),
      bufferedMs: readInt(map['bufferedMs']),
      orderedBufferedMs: readInt(map['orderedBufferedMs']),
      targetBufferMs: readInt(map['targetBufferMs']),
      playbackStretchRatio: readDouble(
        map['playbackStretchRatio'],
        fallback: 1,
      ),
    );
  }

  String get bufferSummary => _formatBuffer(bufferedMs, bufferedFrames);

  String get orderedBufferSummary =>
      _formatBuffer(orderedBufferedMs, orderedBufferedFrames);

  String get targetBufferSummary =>
      _formatBuffer(targetBufferMs, targetBufferFrames);

  Duration get runningDuration {
    if (startedAtMs <= 0) return Duration.zero;
    final startedAt = DateTime.fromMillisecondsSinceEpoch(startedAtMs);
    final elapsed = DateTime.now().difference(startedAt);
    return elapsed.isNegative ? Duration.zero : elapsed;
  }

  static String _formatBuffer(int milliseconds, int frames) {
    if (frames <= 0) return '$milliseconds ms';
    return '$milliseconds ms ($frames 帧)';
  }

  static int readInt(Object? value) {
    if (value is int) return value;
    if (value is num) return value.toInt();
    return int.tryParse(value?.toString() ?? '') ?? 0;
  }

  static int? readIntOrNull(Object? value) {
    if (value == null) return null;
    if (value is int) return value;
    if (value is num) return value.toInt();
    return int.tryParse(value.toString());
  }

  static double readDouble(Object? value, {double fallback = 0}) {
    if (value is double) return value;
    if (value is num) return value.toDouble();
    return double.tryParse(value?.toString() ?? '') ?? fallback;
  }
}
