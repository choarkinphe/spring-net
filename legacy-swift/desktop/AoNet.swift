import AppKit
import WebKit
import UniformTypeIdentifiers
import Darwin

// This executable is only the native host. No shell, command bridge or file-write bridge
// is exposed to web content. The bundled aonet-engine owns all application APIs.
private struct DesktopError: LocalizedError {
    let message: String
    var errorDescription: String? { message }
}

private final class DesktopLog {
    private let queue = DispatchQueue(label: "AoNet.desktop.log")
    private let url: URL
    private let limit = 2 * 1024 * 1024
    private var handle: FileHandle?
    private var size = 0

    init() throws {
        let directory = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Logs/AoNet", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        url = directory.appendingPathComponent("desktop.log")
        try open()
    }

    private func open() throws {
        if !FileManager.default.fileExists(atPath: url.path) {
            guard FileManager.default.createFile(atPath: url.path, contents: nil,
                                                 attributes: [.posixPermissions: 0o600]) else {
                throw DesktopError(message: "无法创建 desktop.log")
            }
        }
        handle = try FileHandle(forWritingTo: url)
        size = Int(try handle!.seekToEnd())
    }

    func line(_ source: String, _ text: String) {
        queue.sync {
            // Never intentionally persist bootstrap credentials, including accidental engine logs.
            let sanitized = text.replacingOccurrences(
                of: "(?i)([\\\"']?(?:token|x-aonet-token)[\\\"']?\\s*[:=]\\s*)[\\\"']?[^\\s,}\\\"']+",
                with: "$1[redacted]", options: .regularExpression)
            let entry = "\(ISO8601DateFormatter().string(from: Date())) [\(source)] \(sanitized)\n"
            let bytes = Data(entry.utf8)
            do {
                if size + bytes.count > limit {
                    try handle?.close()
                    handle = nil
                    let previous = url.appendingPathExtension("1")
                    if FileManager.default.fileExists(atPath: previous.path) {
                        try FileManager.default.removeItem(at: previous)
                    }
                    try FileManager.default.moveItem(at: url, to: previous)
                    try open()
                }
                if handle == nil { try open() }
                try handle?.write(contentsOf: bytes)
                size += bytes.count
            } catch {
                // A logging failure must never stop pipe consumption or deadlock the engine.
                try? handle?.close()
                handle = nil
            }
        }
    }
}

// AppKit's terminateLater can run a modal loop which does not service main-queue
// blocks. Deliver shutdown continuations in the actual application run-loop modes.
private func onApplicationRunLoop(_ action: @escaping () -> Void) {
    RunLoop.main.perform(inModes: [.default, .modalPanel, .eventTracking], block: action)
    CFRunLoopWakeUp(CFRunLoopGetMain())
}

private final class NoRedirect: NSObject, URLSessionTaskDelegate {
    func urlSession(_ session: URLSession, task: URLSessionTask,
                    willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest,
                    completionHandler: @escaping (URLRequest?) -> Void) {
        completionHandler(nil)
    }
}

private final class DesktopApp: NSObject, NSApplicationDelegate, NSWindowDelegate,
                                WKNavigationDelegate, WKUIDelegate, WKDownloadDelegate, WKScriptMessageHandler {
    private var window: NSWindow!
    private var web: WKWebView!
    private var overlay: NSView!
    private var status: NSTextField!
    private var progress: NSProgressIndicator!
    private var process: Process?
    private var input: FileHandle?
    private var lockFD: Int32 = -1
    private var log: DesktopLog?
    private var dataURL: URL!
    private var supportURL: URL!
    private var authorizationURL: URL!
    private var origin: URL?
    private let instanceID = UUID().uuidString
    private var announcedPort: Int?
    private var verified = false
    private var failed = false
    private var checkingQuit = false
    private var terminating = false
    private var readyTimeout: DispatchWorkItem?
    private var killTimeout: Timer?
    private var healthRetry: DispatchWorkItem?
    private var pageTimeout: DispatchWorkItem?
    private let linkWorld = WKContentWorld.world(name: "AoNet.trusted-link-intent")
    private var downloadDestinations: [ObjectIdentifier: (temporary: URL, selected: URL)] = [:]
    private let noRedirect = NoRedirect()
    private lazy var session: URLSession = {
        let config = URLSessionConfiguration.ephemeral
        config.connectionProxyDictionary = [:]
        config.timeoutIntervalForRequest = 2
        config.timeoutIntervalForResource = 3
        config.httpCookieStorage = nil
        config.urlCache = nil
        return URLSession(configuration: config, delegate: noRedirect, delegateQueue: nil)
    }()
    private var downloads: [ObjectIdentifier: WKDownload] = [:]
    private var downloadPanels: [ObjectIdentifier: NSSavePanel] = [:]
    private var blobDownloads = Set<ObjectIdentifier>()
    private var smokePath: URL?
    private var smokeDemo = false
    private var smokeStarted = false
    private var smokeFinished = false
    private var smokeDeadline: DispatchWorkItem?
    private var smokeReport: [String: Any] = [:]

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.regular)
        buildMenu()
        buildWindow()
        do {
            try parseArguments()
            try prepareDirectories()
            guard try acquireLock() else { return }
            log = try DesktopLog()
            try installAuthorizationTemplate()
            try startEngine()
        } catch {
            showFailure(error.localizedDescription)
        }
    }

    private func parseArguments() throws {
        let args = Array(CommandLine.arguments.dropFirst())
        guard !args.isEmpty else { return }
        // Finder may supply a legacy process serial number argument.
        if args.count == 1 && args[0].hasPrefix("-psn_") { return }
        guard (args.count == 2 || args.count == 3), args[0] == "--smoke-test",
              args[1].hasPrefix("/"), (args.count == 2 || args[2] == "--smoke-demo") else {
            throw DesktopError(message: "参数无效。验收用法：--smoke-test <绝对报告路径> [--smoke-demo]")
        }
        smokePath = URL(fileURLWithPath: args[1]).standardizedFileURL
        smokeDemo = args.count == 3
    }

    private func prepareDirectories() throws {
        let fm = FileManager.default
        if let override = ProcessInfo.processInfo.environment["AONET_DESKTOP_DATA_DIR"] {
            guard override.hasPrefix("/"), !override.isEmpty else {
                throw DesktopError(message: "AONET_DESKTOP_DATA_DIR 必须是绝对路径")
            }
            // Acceptance runs keep the lock and authorization beside their isolated data.
            dataURL = URL(fileURLWithPath: override, isDirectory: true).standardizedFileURL
            supportURL = dataURL
        } else {
            supportURL = fm.homeDirectoryForCurrentUser
                .appendingPathComponent("Library/Application Support/AoNet", isDirectory: true)
            dataURL = supportURL.appendingPathComponent("data", isDirectory: true)
        }
        authorizationURL = supportURL.appendingPathComponent("authorized-targets.json")
        for directory in [supportURL!, dataURL!] {
            try fm.createDirectory(at: directory, withIntermediateDirectories: true,
                                   attributes: [.posixPermissions: 0o700])
        }
    }

    private func acquireLock() throws -> Bool {
        let path = supportURL.appendingPathComponent("desktop.lock").path
        lockFD = Darwin.open(path, O_CREAT | O_RDWR | O_CLOEXEC | O_NOFOLLOW, 0o600)
        guard lockFD >= 0 else { throw DesktopError(message: "无法打开桌面实例锁：\(String(cString: strerror(errno)))") }
        if flock(lockFD, LOCK_EX | LOCK_NB) == 0 { return true }
        let code = errno
        Darwin.close(lockFD)
        lockFD = -1
        guard code == EWOULDBLOCK || code == EAGAIN else {
            throw DesktopError(message: "无法获取桌面实例锁：\(String(cString: strerror(code)))")
        }
        if let identifier = Bundle.main.bundleIdentifier {
            let existing = NSRunningApplication.runningApplications(withBundleIdentifier: identifier)
                .first { $0.processIdentifier != ProcessInfo.processInfo.processIdentifier }
            if let existing = existing {
                existing.unhide()
                existing.activate(options: [.activateAllWindows, .activateIgnoringOtherApps])
                // Reopen restores a window minimized or closed by the running application.
                let config = NSWorkspace.OpenConfiguration()
                config.activates = true
                NSWorkspace.shared.openApplication(at: Bundle.main.bundleURL, configuration: config) { _, _ in }
            }
        }
        if smokePath != nil { writeSmokeFailure("已有实例持有 desktop.lock；未启动第二个引擎") }
        NSApp.terminate(nil)
        return false
    }

    private func installAuthorizationTemplate() throws {
        // copyItem is deliberately not overwrite-capable. Never import development data.
        let fm = FileManager.default
        guard !fm.fileExists(atPath: authorizationURL.path) else { return }
        guard let resources = Bundle.main.resourceURL else { throw DesktopError(message: "应用缺少 Resources") }
        do {
            try fm.copyItem(at: resources.appendingPathComponent("config/authorized-targets.json"),
                            to: authorizationURL)
            try fm.setAttributes([.posixPermissions: 0o600], ofItemAtPath: authorizationURL.path)
        } catch {
            if !fm.fileExists(atPath: authorizationURL.path) { throw error }
        }
    }

    private func startEngine() throws {
        guard let resources = Bundle.main.resourceURL, let executable = Bundle.main.executableURL else {
            throw DesktopError(message: "应用包不完整：无法定位 Resources 或可执行目录")
        }
        let helper = executable.deletingLastPathComponent().appendingPathComponent("aonet-engine")
        guard FileManager.default.isExecutableFile(atPath: helper.path) else {
            throw DesktopError(message: "应用包缺少可执行的 Contents/MacOS/aonet-engine")
        }
        let child = Process()
        child.executableURL = helper
        child.arguments = []
        child.currentDirectoryURL = resources
        var env = ProcessInfo.processInfo.environment
        env["AONET_ROOT"] = resources.path
        env["AONET_DATA_DIR"] = dataURL.path
        env["AONET_AUTH_FILE"] = authorizationURL.path
        env["AONET_PORT"] = "0"
        env["AONET_INSTANCE_ID"] = instanceID
        env["AONET_MANAGED"] = "1"
        child.environment = env
        let stdinPipe = Pipe(), stdoutPipe = Pipe(), stderrPipe = Pipe()
        child.standardInput = stdinPipe
        child.standardOutput = stdoutPipe
        child.standardError = stderrPipe
        input = stdinPipe.fileHandleForWriting // EOF is the parent-lifetime contract.
        child.terminationHandler = { [weak self] child in
            onApplicationRunLoop { self?.engineExited(child) }
        }
        process = child
        do { try child.run() } catch {
            process = nil
            try? input?.close()
            input = nil
            throw error
        }
        try? stdinPipe.fileHandleForReading.close()
        try? stdoutPipe.fileHandleForWriting.close()
        try? stderrPipe.fileHandleForWriting.close()
        drain(stdoutPipe.fileHandleForReading, source: "stdout", parseReady: true)
        drain(stderrPipe.fileHandleForReading, source: "stderr", parseReady: false)
        log?.line("desktop", "已启动本实例引擎，等待 READY 和 health 校验")
        let timeout = DispatchWorkItem { [weak self] in
            guard let self = self, !self.verified, !self.terminating else { return }
            self.showFailure("引擎在 20 秒内未完成就绪校验。请检查日志和授权配置。")
        }
        readyTimeout = timeout
        DispatchQueue.main.asyncAfter(deadline: .now() + 20, execute: timeout)
    }

    private func drain(_ handle: FileHandle, source: String, parseReady: Bool) {
        let logger = log
        DispatchQueue.global(qos: .utility).async { [weak self] in
            var line = Data()
            var oversized = false
            defer { try? handle.close() }
            func consume() {
                if oversized {
                    logger?.line(source, "[超过 8192 字节的一行已丢弃]")
                } else if !line.isEmpty {
                    if line.last == 13 { line.removeLast() }
                    let text = String(decoding: line, as: UTF8.self)
                    if parseReady && text.hasPrefix("AONET_READY ") {
                        // READY contains no secret. Do not log unvalidated payloads.
                        let payload = Data(line.dropFirst("AONET_READY ".utf8.count))
                        DispatchQueue.main.async { self?.receiveReady(payload) }
                        logger?.line(source, "AONET_READY [收到就绪声明]")
                    } else {
                        logger?.line(source, text)
                    }
                }
                line.removeAll(keepingCapacity: true)
                oversized = false
            }
            // A single POSIX read returns as soon as *any* pipe bytes are available.
            // FileHandle.read(upToCount:) can wait to fill the requested buffer on this SDK.
            var buffer = [UInt8](repeating: 0, count: 4096)
            while true {
                let count = buffer.withUnsafeMutableBytes { bytes in
                    Darwin.read(handle.fileDescriptor, bytes.baseAddress!, bytes.count)
                }
                if count == 0 { break }
                if count < 0 {
                    if errno == EINTR { continue }
                    logger?.line("desktop", "\(source) 读取结束：\(String(cString: strerror(errno)))")
                    break
                }
                for byte in buffer.prefix(count) {
                    if byte == 10 { consume() }
                    else if !oversized {
                        if line.count < 8192 { line.append(byte) }
                        else { oversized = true; line.removeAll(keepingCapacity: true) }
                    }
                }
            }
            // An unterminated tail is log output, never a valid READY frame.
            if !line.isEmpty || oversized {
                logger?.line(source, oversized ? "[超长尾行已丢弃]" : String(decoding: line, as: UTF8.self))
            }
        }
    }

    private func receiveReady(_ payload: Data) {
        guard !failed, !terminating, !verified, announcedPort == nil, process?.isRunning == true else { return }
        guard let value = try? JSONSerialization.jsonObject(with: payload) as? [String: Any],
              Set(value.keys) == Set(["protocol", "port", "instance_id"]),
              let proto = value["protocol"] as? NSNumber, CFGetTypeID(proto) != CFBooleanGetTypeID(), proto.doubleValue == 1,
              let port = value["port"] as? NSNumber, CFGetTypeID(port) != CFBooleanGetTypeID(),
              port.doubleValue == Double(port.intValue), (1...65535).contains(port.intValue),
              value["instance_id"] as? String == instanceID else {
            showFailure("引擎的 AONET_READY 协议、端口或实例标识无效。")
            return
        }
        announcedPort = port.intValue
        checkHealth(port: port.intValue)
    }

    private func checkHealth(port: Int) {
        guard !failed, !terminating, process?.isRunning == true else { return }
        let candidate = URL(string: "http://127.0.0.1:\(port)")!
        requestJSON(candidate.appendingPathComponent("api/health")) { [weak self] result in
            guard let self = self, !self.failed, !self.terminating,
                  self.process?.isRunning == true, self.announcedPort == port else { return }
            if case .success(let object) = result, let health = object as? [String: Any],
               health["app"] as? String == "aonet", health["version"] as? String == "0.1.0",
               let ready = health["ready"] as? NSNumber, CFGetTypeID(ready) == CFBooleanGetTypeID(), ready.boolValue,
               health["instance_id"] as? String == self.instanceID {
                self.verified = true
                self.origin = candidate
                self.readyTimeout?.cancel()
                self.status.stringValue = "引擎已验证，正在加载工作台…"
                self.web.load(URLRequest(url: candidate.appendingPathComponent("")))
            } else {
                let retry = DispatchWorkItem { [weak self] in self?.checkHealth(port: port) }
                self.healthRetry = retry
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.25, execute: retry)
            }
        }
    }

    private func requestJSON(_ url: URL, completion: @escaping (Result<Any, Error>) -> Void) {
        var request = URLRequest(url: url)
        request.cachePolicy = .reloadIgnoringLocalCacheData
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        session.dataTask(with: request) { data, response, error in
            let result: Result<Any, Error>
            do {
                if let error = error { throw error }
                guard let response = response as? HTTPURLResponse, response.statusCode == 200,
                      response.url == url, let data = data, data.count <= 1024 * 1024 else {
                    throw DesktopError(message: "本机 API 返回无效响应（不允许重定向）")
                }
                result = .success(try JSONSerialization.jsonObject(with: data, options: [.fragmentsAllowed]))
            } catch { result = .failure(error) }
            onApplicationRunLoop { completion(result) }
        }.resume()
    }

    private func buildWindow() {
        window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 1280, height: 840),
                          styleMask: [.titled, .closable, .miniaturizable, .resizable],
                          backing: .buffered, defer: false)
        window.title = "AoNet"
        window.minSize = NSSize(width: 900, height: 620)
        window.isReleasedWhenClosed = false
        window.delegate = self
        let config = WKWebViewConfiguration()
        config.websiteDataStore = .nonPersistent()
        config.preferences.javaScriptCanOpenWindowsAutomatically = false
        // Only an isolated-world, trusted DOM click can ask to open an external link.
        // navigationType == linkActivated alone also includes JavaScript element.click().
        let links = """
        addEventListener('click', event => {
          if (!event.isTrusted || event.defaultPrevented) return;
          const anchor = event.composedPath().find(n => n instanceof HTMLAnchorElement);
          if (!anchor) return;
          let url; try { url = new URL(anchor.href); } catch { return; }
          if (!['http:', 'https:'].includes(url.protocol) || url.origin === location.origin) return;
          event.preventDefault();
          window.webkit.messageHandlers.externalLink.postMessage(url.href);
        }, true);
        """
        config.userContentController.add(self, contentWorld: linkWorld, name: "externalLink")
        config.userContentController.addUserScript(WKUserScript(source: links, injectionTime: .atDocumentStart,
                                                               forMainFrameOnly: false, in: linkWorld))
        web = WKWebView(frame: .zero, configuration: config)
        web.navigationDelegate = self
        web.uiDelegate = self
        web.allowsBackForwardNavigationGestures = false
        let root = NSView()
        window.contentView = root
        web.translatesAutoresizingMaskIntoConstraints = false
        root.addSubview(web)
        overlay = NSView()
        overlay.wantsLayer = true
        overlay.layer?.backgroundColor = NSColor.windowBackgroundColor.cgColor
        overlay.translatesAutoresizingMaskIntoConstraints = false
        root.addSubview(overlay)
        status = NSTextField(wrappingLabelWithString: "正在启动 AoNet 本机引擎…")
        status.alignment = .center
        status.font = .systemFont(ofSize: 16)
        progress = NSProgressIndicator()
        progress.style = .spinning
        progress.startAnimation(nil)
        let stack = NSStackView(views: [progress, status])
        stack.orientation = .vertical
        stack.spacing = 20
        stack.translatesAutoresizingMaskIntoConstraints = false
        overlay.addSubview(stack)
        NSLayoutConstraint.activate([
            web.leadingAnchor.constraint(equalTo: root.leadingAnchor), web.trailingAnchor.constraint(equalTo: root.trailingAnchor),
            web.topAnchor.constraint(equalTo: root.topAnchor), web.bottomAnchor.constraint(equalTo: root.bottomAnchor),
            overlay.leadingAnchor.constraint(equalTo: root.leadingAnchor), overlay.trailingAnchor.constraint(equalTo: root.trailingAnchor),
            overlay.topAnchor.constraint(equalTo: root.topAnchor), overlay.bottomAnchor.constraint(equalTo: root.bottomAnchor),
            stack.centerXAnchor.constraint(equalTo: overlay.centerXAnchor), stack.centerYAnchor.constraint(equalTo: overlay.centerYAnchor),
            stack.widthAnchor.constraint(lessThanOrEqualToConstant: 620)
        ])
        window.center()
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    private func buildMenu() {
        let bar = NSMenu()
        func menu(_ title: String) -> NSMenu {
            let item = NSMenuItem()
            let submenu = NSMenu(title: title)
            item.submenu = submenu
            bar.addItem(item)
            return submenu
        }
        func item(_ menu: NSMenu, _ title: String, _ action: Selector, _ key: String = "", target: AnyObject? = nil) {
            let item = NSMenuItem(title: title, action: action, keyEquivalent: key)
            item.target = target
            menu.addItem(item)
        }
        let app = menu("AoNet")
        item(app, "关于 AoNet", #selector(about), target: self)
        app.addItem(.separator())
        item(app, "隐藏 AoNet", #selector(NSApplication.hide(_:)), "h")
        item(app, "显示全部", #selector(NSApplication.unhideAllApplications(_:)))
        app.addItem(.separator())
        item(app, "退出 AoNet", #selector(NSApplication.terminate(_:)), "q")
        let edit = menu("编辑")
        item(edit, "撤销", Selector(("undo:")), "z")
        let redo = NSMenuItem(title: "重做", action: Selector(("redo:")), keyEquivalent: "z")
        redo.keyEquivalentModifierMask = [.command, .shift]
        edit.addItem(redo)
        edit.addItem(.separator())
        item(edit, "剪切", #selector(NSText.cut(_:)), "x")
        item(edit, "拷贝", #selector(NSText.copy(_:)), "c")
        item(edit, "粘贴", #selector(NSText.paste(_:)), "v")
        item(edit, "全选", #selector(NSText.selectAll(_:)), "a")
        item(menu("显示"), "重新加载", #selector(reload), "r", target: self)
        let windows = menu("窗口")
        item(windows, "最小化", #selector(NSWindow.performMiniaturize(_:)), "m")
        item(windows, "缩放", #selector(NSWindow.performZoom(_:)))
        NSApp.windowsMenu = windows
        let help = menu("帮助")
        item(help, "打开数据目录", #selector(openData), target: self)
        item(help, "显示桌面日志", #selector(openLog), target: self)
        item(help, "显示授权配置", #selector(openAuthorization), target: self)
        NSApp.helpMenu = help
        NSApp.mainMenu = bar
    }

    @objc private func about() {
        NSApp.orderFrontStandardAboutPanel(options: [.applicationName: "AoNet", .applicationVersion: "0.1.0",
                                                     .credits: NSAttributedString(string: "本机网络测试工作台。仅对已授权目标开展测试。")])
    }
    @objc private func openData() { if let url = dataURL { NSWorkspace.shared.open(url) } }
    @objc private func openLog() {
        let url = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/Logs/AoNet/desktop.log")
        NSWorkspace.shared.activateFileViewerSelecting([url])
    }
    @objc private func openAuthorization() {
        if let url = authorizationURL { NSWorkspace.shared.activateFileViewerSelecting([url]) }
    }
    @objc private func reload() {
        guard verified, !failed, !terminating, let origin = origin, process?.isRunning == true else { return }
        web.load(URLRequest(url: origin))
    }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        guard !terminating else { return false }
        window.deminiaturize(nil)
        window.makeKeyAndOrderFront(nil)
        sender.activate(ignoringOtherApps: true)
        return true
    }
    func windowShouldClose(_ sender: NSWindow) -> Bool {
        NSApp.terminate(nil) // Keep the last window available if quit confirmation is cancelled.
        return false
    }
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }

    private func sameOrigin(_ url: URL?) -> Bool {
        guard verified, let url = url, let origin = origin,
              url.user == nil, url.password == nil else { return false }
        return url.scheme == "http" && url.host == "127.0.0.1" && url.port == origin.port
    }
    private func trustedFrame(_ frame: WKFrameInfo) -> Bool {
        guard let port = origin?.port else { return false }
        let security = frame.securityOrigin
        return sameOrigin(web.url) && security.protocol == "http" && security.host == "127.0.0.1" && security.port == port
    }
    private func localBlob(_ url: URL?, frame: WKFrameInfo? = nil) -> Bool {
        guard let url = url, url.scheme == "blob", sameOrigin(web.url),
              sameOrigin(URL(string: String(url.absoluteString.dropFirst(5)))) else { return false }
        return frame.map(trustedFrame) ?? true
    }
    func userContentController(_ userContentController: WKUserContentController, didReceive message: WKScriptMessage) {
        guard message.name == "externalLink", message.world == linkWorld, !terminating, !failed,
              trustedFrame(message.frameInfo), let text = message.body as? String, text.utf8.count <= 8192,
              let url = URL(string: text), ["http", "https"].contains(url.scheme ?? ""),
              !sameOrigin(url), url.host != nil, url.user == nil, url.password == nil else { return }
        NSWorkspace.shared.open(url)
    }

    func webView(_ webView: WKWebView, decidePolicyFor navigationAction: WKNavigationAction,
                 decisionHandler: @escaping (WKNavigationActionPolicy) -> Void) {
        guard !terminating, !failed else { decisionHandler(.cancel); return }
        let url = navigationAction.request.url
        if sameOrigin(url) {
            if navigationAction.shouldPerformDownload { decisionHandler(.download) }
            else if navigationAction.targetFrame == nil {
                // A same-origin target=_blank stays inside our one controlled web view.
                decisionHandler(.cancel)
                if trustedFrame(navigationAction.sourceFrame) { webView.load(navigationAction.request) }
            } else { decisionHandler(.allow) }
        } else if navigationAction.shouldPerformDownload && localBlob(url, frame: navigationAction.sourceFrame) {
            decisionHandler(.download)
        } else {
            // Script navigation and HTTP redirects are never sent to the system browser.
            decisionHandler(.cancel)
        }
    }

    func webView(_ webView: WKWebView, decidePolicyFor navigationResponse: WKNavigationResponse,
                 decisionHandler: @escaping (WKNavigationResponsePolicy) -> Void) {
        guard !terminating, !failed, sameOrigin(navigationResponse.response.url) || localBlob(navigationResponse.response.url) else {
            decisionHandler(.cancel)
            return
        }
        let disposition = (navigationResponse.response as? HTTPURLResponse)?
            .value(forHTTPHeaderField: "Content-Disposition")?
            .split(separator: ";", maxSplits: 1).first?.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        decisionHandler(disposition == "attachment" || !navigationResponse.canShowMIMEType ? .download : .allow)
    }

    func webView(_ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration,
                 for navigationAction: WKNavigationAction, windowFeatures: WKWindowFeatures) -> WKWebView? {
        // Navigation policy normally handles this first. No uncontrolled popup is ever created.
        return nil
    }
    func webView(_ webView: WKWebView, didStartProvisionalNavigation navigation: WKNavigation!) {
        pageTimeout?.cancel()
        let timeout = DispatchWorkItem { [weak self] in
            self?.showFailure("工作台页面在 20 秒内未完成加载。请查看日志后重新启动。")
        }
        pageTimeout = timeout
        DispatchQueue.main.asyncAfter(deadline: .now() + 20, execute: timeout)
    }
    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        guard !failed, !terminating, sameOrigin(webView.url) else { return }
        pageTimeout?.cancel()
        overlay.isHidden = true
        window.makeFirstResponder(web)
        if smokePath != nil && !smokeStarted { startSmoke() }
    }
    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        if (error as NSError).code != NSURLErrorCancelled { showFailure("工作台加载失败：\(error.localizedDescription)") }
    }
    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        if (error as NSError).code != NSURLErrorCancelled { showFailure("工作台加载失败：\(error.localizedDescription)") }
    }
    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) { showFailure("WebKit 页面进程已退出，请退出后重新打开 AoNet。") }

    func webView(_ webView: WKWebView, runOpenPanelWith parameters: WKOpenPanelParameters,
                 initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping ([URL]?) -> Void) {
        guard trustedFrame(frame), !terminating else { completionHandler(nil); return }
        let panel = NSOpenPanel()
        panel.title = "导入 AoNet JSON"
        panel.allowedContentTypes = [.json]
        panel.allowsOtherFileTypes = false
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = parameters.allowsMultipleSelection
        panel.beginSheetModal(for: window) { response in completionHandler(response == .OK ? panel.urls : nil) }
    }

    private func attachDownload(_ download: WKDownload, blob: Bool) {
        let id = ObjectIdentifier(download)
        downloads[id] = download
        if blob { blobDownloads.insert(id) }
        download.delegate = self
    }
    func webView(_ webView: WKWebView, navigationAction: WKNavigationAction, didBecome download: WKDownload) {
        let blob = localBlob(navigationAction.request.url, frame: navigationAction.sourceFrame)
        guard sameOrigin(navigationAction.request.url) || blob else { download.cancel { _ in }; return }
        attachDownload(download, blob: blob)
    }
    func webView(_ webView: WKWebView, navigationResponse: WKNavigationResponse, didBecome download: WKDownload) {
        let blob = localBlob(navigationResponse.response.url)
        guard sameOrigin(navigationResponse.response.url) || blob else { download.cancel { _ in }; return }
        attachDownload(download, blob: blob)
    }
    func download(_ download: WKDownload, decideDestinationUsing response: URLResponse,
                  suggestedFilename: String, completionHandler: @escaping (URL?) -> Void) {
        let id = ObjectIdentifier(download)
        guard !terminating, downloads[id] != nil,
              sameOrigin(response.url) || (blobDownloads.contains(id) && localBlob(response.url)) else {
            completionHandler(nil); forgetDownload(download); return
        }
        let panel = NSSavePanel()
        // Strip both POSIX and Windows path components and control characters.
        let basename = suggestedFilename.replacingOccurrences(of: "\\", with: "/").components(separatedBy: "/").last ?? ""
        let cleaned = basename.components(separatedBy: .controlCharacters).joined()
        panel.nameFieldStringValue = cleaned.isEmpty || cleaned == "." || cleaned == ".." ? "aonet-export.json" : String(cleaned.prefix(240))
        panel.canCreateDirectories = true
        panel.title = "保存 AoNet 导出文件"
        downloadPanels[id] = panel
        panel.beginSheetModal(for: window) { [weak self] response in
            guard let self = self else { completionHandler(nil); return }
            self.downloadPanels.removeValue(forKey: id)
            guard response == .OK, let selected = panel.url, !self.terminating else {
                completionHandler(nil); self.forgetDownload(download); return
            }
            // WKDownload requires a nonexistent destination. Stage beside the user-selected
            // file, then atomically replace it only after a successful download.
            let temporary = selected.deletingLastPathComponent().appendingPathComponent(".aonet-\(UUID().uuidString).download")
            self.downloadDestinations[id] = (temporary, selected)
            completionHandler(temporary)
        }
    }
    func download(_ download: WKDownload, willPerformHTTPRedirection response: HTTPURLResponse,
                  newRequest request: URLRequest, decisionHandler: @escaping (WKDownload.RedirectPolicy) -> Void) {
        decisionHandler(sameOrigin(request.url) ? .allow : .cancel)
    }
    func download(_ download: WKDownload, didReceive challenge: URLAuthenticationChallenge,
                  completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void) {
        completionHandler(.cancelAuthenticationChallenge, nil)
    }
    func downloadDidFinish(_ download: WKDownload) {
        let id = ObjectIdentifier(download)
        if let destination = downloadDestinations[id] {
            do {
                guard Darwin.rename(destination.temporary.path, destination.selected.path) == 0 else {
                    throw DesktopError(message: "无法保存导出：\(String(cString: strerror(errno)))")
                }
            } catch {
                if !terminating {
                    let alert = NSAlert()
                    alert.messageText = "导出保存失败"
                    alert.informativeText = error.localizedDescription
                    alert.beginSheetModal(for: window)
                }
            }
        }
        forgetDownload(download)
    }
    func download(_ download: WKDownload, didFailWithError error: Error, resumeData: Data?) {
        forgetDownload(download)
        guard !terminating, (error as NSError).code != NSURLErrorCancelled else { return }
        let alert = NSAlert()
        alert.messageText = "导出失败"
        alert.informativeText = error.localizedDescription
        alert.beginSheetModal(for: window)
    }
    private func forgetDownload(_ download: WKDownload) {
        let id = ObjectIdentifier(download)
        downloads.removeValue(forKey: id)
        if let destination = downloadDestinations.removeValue(forKey: id) {
            try? FileManager.default.removeItem(at: destination.temporary)
        }
        blobDownloads.remove(id)
        downloadPanels.removeValue(forKey: id)
    }

    private func showFailure(_ message: String) {
        guard !terminating, !failed else { return }
        failed = true
        verified = false
        readyTimeout?.cancel()
        healthRetry?.cancel()
        pageTimeout?.cancel()
        web.stopLoading()
        overlay.isHidden = false
        progress.stopAnimation(nil)
        status.stringValue = "AoNet 无法继续运行\n\(message)\n可从“帮助”菜单查看日志和配置。"
        log?.line("desktop", message)
        stopHelper()
        if smokePath != nil {
            writeSmokeFailure(message) // completeSmoke schedules one delayed quit request.
        } else {
            let alert = NSAlert()
            alert.alertStyle = .critical
            alert.messageText = "AoNet 启动或运行失败"
            alert.informativeText = message
            alert.addButton(withTitle: "查看日志")
            alert.addButton(withTitle: "留在窗口")
            alert.beginSheetModal(for: window) { [weak self] response in
                if response == .alertFirstButtonReturn { self?.openLog() }
            }
        }
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        if terminating || checkingQuit { return .terminateLater }
        checkingQuit = true
        // A run-loop block survives AppKit's terminateLater modal loop; a main-queue
        // block can be starved here and leave an otherwise idle shell running forever.
        onApplicationRunLoop { [weak self] in self?.checkBeforeQuit() }
        return .terminateLater
    }
    private func checkBeforeQuit() {
        guard let origin = origin, process?.isRunning == true, !failed else { beginTermination(); return }
        requestJSON(origin.appendingPathComponent("api/state")) { [weak self] result in
            guard let self = self, self.checkingQuit, !self.terminating else { return }
            if self.smokeFinished { self.beginTermination(); return }
            let alert = NSAlert()
            var needsConfirmation = true
            switch result {
            case .success(let value):
                if value is NSNull { needsConfirmation = false }
                else if let run = value as? [String: Any], let state = run["status"] as? String {
                    let active = ["running", "stopping"].contains(state)
                    alert.messageText = active ? "停止当前测试并退出？" : "退出 AoNet？"
                    alert.informativeText = (active ? "当前测试仍在进行，退出会取消活动任务。\n" : "") + "最近结果仅保存在内存中，退出后无法恢复。如需保留，请取消退出并先导出 JSON 或 CSV。"
                } else {
                    alert.messageText = "无法确认当前测试状态，仍要退出？"
                    alert.informativeText = "退出将停止本实例引擎；最近结果仅在内存中。可以取消后核对状态并导出。"
                }
            case .failure:
                alert.messageText = "无法确认当前测试状态，仍要退出？"
                alert.informativeText = "本机后台未响应。退出将停止本实例引擎，未导出的内存结果会丢失。"
            }
            if !needsConfirmation { self.beginTermination(); return }
            alert.addButton(withTitle: "取消，返回工作台")
            alert.addButton(withTitle: "停止并退出")
            alert.beginSheetModal(for: self.window) { response in
                if response == .alertSecondButtonReturn { self.beginTermination() }
                else { self.checkingQuit = false; NSApp.reply(toApplicationShouldTerminate: false) }
            }
        }
    }
    private func beginTermination() {
        terminating = true
        readyTimeout?.cancel()
        healthRetry?.cancel()
        pageTimeout?.cancel()
        smokeDeadline?.cancel()
        web.stopLoading()
        web.configuration.userContentController.removeAllScriptMessageHandlers()
        for panel in Array(downloadPanels.values) { panel.cancel(nil) }
        for download in Array(downloads.values) { download.cancel { _ in } }
        stopHelper()
        if process == nil { finishTermination() }
        // Otherwise only the terminationHandler is allowed to release the lock and reply.
    }
    private func stopHelper() {
        try? input?.close()
        input = nil
        guard let child = process else { return }
        if child.isRunning { child.terminate() }
        if killTimeout == nil {
            let timeout = Timer(timeInterval: 5, repeats: false) { [weak self, weak child] _ in
                guard let self = self, let child = child, self.process === child, child.isRunning else { return }
                self.log?.line("desktop", "本实例引擎未在 5 秒内退出，发送 SIGKILL")
                // Only the retained Process we launched; never discover/kill by port or stale PID.
                Darwin.kill(child.processIdentifier, SIGKILL)
            }
            killTimeout = timeout
            for mode in [RunLoop.Mode.default, .modalPanel, .eventTracking] {
                RunLoop.main.add(timeout, forMode: mode)
            }
        }
    }
    private func engineExited(_ child: Process) {
        guard process === child else { return }
        killTimeout?.invalidate()
        killTimeout = nil
        process = nil
        try? input?.close()
        input = nil
        log?.line("desktop", "本实例引擎已退出（status=\(child.terminationStatus)）")
        if terminating { finishTermination() }
        else if !failed { showFailure("本实例引擎意外退出（状态 \(child.terminationStatus)）。") }
    }
    private func finishTermination() {
        guard process == nil else { return }
        session.invalidateAndCancel()
        if lockFD >= 0 { flock(lockFD, LOCK_UN); Darwin.close(lockFD); lockFD = -1 }
        NSApp.reply(toApplicationShouldTerminate: true)
    }

    // Smoke execution is opt-in, has no persistent JS bridge, and returns a fixed allowlist
    // of report fields. Public presets are read, never tested automatically.
    private func startSmoke() {
        smokeStarted = true
        let timeout = DispatchWorkItem { [weak self] in
            guard let self = self, !self.smokeFinished else { return }
            self.completeSmoke(ok: false, error: "页面验收超时")
        }
        smokeDeadline = timeout
        DispatchQueue.main.asyncAfter(deadline: .now() + 35, execute: timeout)
        pollSmoke(attempt: 0)
    }
    private func pollSmoke(attempt: Int) {
        guard !smokeFinished, !terminating, !failed else { return }
        let script = """
        (() => ({title: document.title, ua: navigator.userAgent, origin: location.origin,
          bootstrapped: !!document.querySelector('#new-list') && !document.querySelector('#new-list').disabled &&
            !!document.querySelector('#refresh') && !document.querySelector('#refresh').disabled &&
            document.querySelectorAll('#inventory .list-card').length > 0,
          inventoryText: document.querySelector('#inventory-note')?.textContent || ''}))()
        """
        web.evaluateJavaScript(script) { [weak self] value, error in
            guard let self = self, !self.smokeFinished, !self.terminating else { return }
            guard error == nil, let page = value as? [String: Any], page["bootstrapped"] as? Bool == true else {
                if attempt < 80 {
                    DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) { self.pollSmoke(attempt: attempt + 1) }
                } else { self.completeSmoke(ok: false, error: "前端未完成初始化") }
                return
            }
            for field in ["title", "ua", "origin", "bootstrapped"] { self.smokeReport[field] = page[field] }
            self.smokeReport["windowVisible"] = self.window.isVisible && !self.window.isMiniaturized
            // Promise-aware WebKit evaluation: fetch bootstrap without transferring its token to Swift.
            let inspect = """
            const response = await fetch('/api/bootstrap', {cache:'no-store', redirect:'error'});
            if (!response.ok) throw new Error('bootstrap failed');
            const b = await response.json();
            return {targetCount: b.lists.filter(x => x.builtin).reduce((n,x) => n+x.targets.length,0),
                    listCount: b.lists.length, authorizedCount: b.authorized.length};
            """
            self.web.callAsyncJavaScript(inspect, arguments: [:], in: nil, in: .page) { [weak self] result in
                guard let self = self, !self.smokeFinished else { return }
                guard case .success(let value) = result, let counts = value as? [String: Any] else {
                    self.completeSmoke(ok: false, error: "bootstrap fetch 检查失败"); return
                }
                for field in ["targetCount", "listCount", "authorizedCount"] { self.smokeReport[field] = counts[field] }
                guard counts["targetCount"] as? Int == 55,
                      (self.smokeReport["title"] as? String)?.contains("AoNet") == true,
                      self.smokeReport["origin"] as? String == self.origin?.absoluteString,
                      self.smokeReport["windowVisible"] as? Bool == true else {
                    self.completeSmoke(ok: false, error: "窗口、标题、origin 或 55 站点检查未通过"); return
                }
                if self.smokeDemo { self.runSmokeDemo() }
                else { self.completeSmoke(ok: true) }
            }
        }
    }

    private func runSmokeDemo() {
        // Only exact built-in IDs with numeric loopback URLs. One concurrent request, 1 RPS,
        // two-second runs, bounded polling; no public probe or inventory writes.
        let script = """
        const get = async path => {
          const r = await fetch(path, {cache:'no-store',redirect:'error',signal:AbortSignal.timeout(3000)});
          if (!r.ok) throw new Error('read failed'); return r.json();
        };
        const b = await get('/api/bootstrap');
        const post = async (path, body) => {
          const r = await fetch(path, {method:'POST',redirect:'error',signal:AbortSignal.timeout(3000),
            headers:{'Content-Type':'application/json','X-AoNet-Token':b.token},body:JSON.stringify(body)});
          if (!r.ok) throw new Error('write failed'); return r.json();
        };
        const sleep = ms => new Promise(r => setTimeout(r,ms));
        const initial = await get('/api/state');
        if (initial && ['running','stopping'].includes(initial.status)) throw new Error('existing active run');
        const reports = [];
        for (const [id,mode] of [['demo-http','http'],['demo-tcp','tcp']]) {
          const p = b.authorized.find(p => p.id === id);
          if (!p || !p.modes.includes(mode)) throw new Error('missing local demo');
          const u = new URL(p.url);
          if (u.protocol !== 'http:' || u.hostname !== '127.0.0.1' || !u.port || u.username || u.password)
            throw new Error('non-loopback demo rejected');
          let owned = null;
          try {
            const started = await post('/api/runs', {mode, list_ids:[],target_ids:[],authorized_ids:[id],
              concurrency:1,rps:1,duration_secs:2,timeout_secs:1,ramp_secs:0,fresh_connections:false});
            owned = started.id;
            await sleep(1200);
            let current = await get('/api/state');
            if (current?.id !== owned) throw new Error('run identity changed');
            await post('/api/stop', {});
            for (let i=0;i<16;i++) {
              current = await get('/api/state');
              if (current?.id !== owned) throw new Error('run identity changed');
              if (!['running','stopping'].includes(current.status)) break;
              await sleep(150);
            }
            const verified = current.mode === mode && current.targets?.length === 1 &&
              current.targets[0].id === id && current.attempted > 0 && current.completed > 0 &&
              current.errors === 0 && !['running','stopping','failed'].includes(current.status) &&
              (mode !== 'http' || current.good_status > 0);
            reports.push({id,mode,verified,status:current.status,attempted:current.attempted,completed:current.completed});
            if (!verified) throw new Error('demo verification failed');
          } finally {
            const current = await get('/api/state');
            if (owned && current?.id === owned && ['running','stopping'].includes(current.status))
              await post('/api/stop', {});
          }
        }
        return reports;
        """
        web.callAsyncJavaScript(script, arguments: [:], in: nil, in: .page) { [weak self] result in
            guard let self = self, !self.smokeFinished else { return }
            if case .success(let value) = result, let reports = value as? [[String: Any]], reports.count == 2 {
                self.smokeReport["demos"] = reports.map { report in
                    report.filter { ["id", "mode", "verified", "status", "attempted", "completed"].contains($0.key) }
                }
                self.completeSmoke(ok: reports.allSatisfy { $0["verified"] as? Bool == true })
            } else { self.completeSmoke(ok: false, error: "本机 HTTP/TCP 演示验证失败（详情不含令牌）") }
        }
    }
    private func writeSmokeFailure(_ message: String) { completeSmoke(ok: false, error: message) }
    private func completeSmoke(ok: Bool, error: String? = nil) {
        guard !smokeFinished, let path = smokePath else { return }
        smokeFinished = true
        smokeDeadline?.cancel()
        smokeReport["ok"] = ok
        smokeReport["windowVisible"] = window?.isVisible ?? false
        smokeReport["demoRequested"] = smokeDemo
        if let error = error { smokeReport["error"] = String(error.prefix(1000)) }
        do {
            let data = try JSONSerialization.data(withJSONObject: smokeReport, options: [.prettyPrinted, .sortedKeys])
            try data.write(to: path, options: [.atomic])
            log?.line("desktop", "smoke 报告已写入（ok=\(ok)）")
        } catch { log?.line("desktop", "无法写入 smoke 报告：\(error.localizedDescription)") }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.75) { NSApp.terminate(nil) }
    }
}

let app = NSApplication.shared
private let delegate = DesktopApp()
app.delegate = delegate
app.run()
