import AppKit

let app = NSApplication.shared
let delegate = AppDelegate(options: LaunchOptions(arguments: CommandLine.arguments))
app.delegate = delegate
app.setActivationPolicy(.accessory)
app.run()
