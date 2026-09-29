// Draws Yplayer's app icon with Core Graphics and builds an .icns with iconutil.
// Usage: swift scripts/make-icon.swift <out.icns> [preview.png]
// No SF Symbols: their license excludes app icons. The artwork is full bleed: macOS 26 masks a
// full-bleed legacy icon to its own rounded shape, but shrinks any other shape onto a gray plate
// (the native .icon format needs Xcode's actool, which the Command Line Tools lack).
import AppKit
import CoreGraphics
import Foundation

func color(_ hex: UInt32, _ alpha: CGFloat = 1) -> CGColor {
    CGColor(
        srgbRed: CGFloat((hex >> 16) & 0xFF) / 255, green: CGFloat((hex >> 8) & 0xFF) / 255,
        blue: CGFloat(hex & 0xFF) / 255, alpha: alpha)
}

func gradient(_ stops: [(CGColor, CGFloat)]) -> CGGradient {
    CGGradient(
        colorsSpace: CGColorSpace(name: CGColorSpace.sRGB), colors: stops.map(\.0) as CFArray,
        locations: stops.map(\.1))!
}

/// Draws the icon in a 1024-unit space scaled to `px` pixels. At 32 px and below the orb's
/// highlight is dropped and the bars grow, so the waveform still reads.
func render(px: Int) -> CGImage {
    let small = px <= 32
    let ctx = CGContext(
        data: nil, width: px, height: px, bitsPerComponent: 8, bytesPerRow: 0,
        space: CGColorSpace(name: CGColorSpace.sRGB)!,
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    ctx.scaleBy(x: CGFloat(px) / 1024, y: CGFloat(px) / 1024)
    ctx.interpolationQuality = .high
    // The design is laid out on the 824-unit icon grid (100...924), stretched over the canvas.
    let k: CGFloat = 1024 / 824
    ctx.scaleBy(x: k, y: k)
    ctx.translateBy(x: -100, y: -100)

    // Background.
    ctx.saveGState()
    ctx.clip(to: CGRect(x: 100, y: 100, width: 824, height: 824))
    ctx.drawLinearGradient(
        gradient([(color(0x24104F), 0), (color(0x5B1E8C), 0.45), (color(0xD2367E), 1)]),
        start: CGPoint(x: 260, y: 924), end: CGPoint(x: 780, y: 100),
        options: [.drawsBeforeStartLocation, .drawsAfterEndLocation])
    // A warm glow behind the orb.
    ctx.drawRadialGradient(
        gradient([(color(0xFF7AB6, 0.55), 0), (color(0xFF7AB6, 0), 1)]),
        startCenter: CGPoint(x: 512, y: 470), startRadius: 0,
        endCenter: CGPoint(x: 512, y: 470), endRadius: 420, options: [])
    ctx.restoreGState()

    // The glass orb.
    let orb = CGRect(x: 512 - 280, y: 512 - 280, width: 560, height: 560)
    ctx.saveGState()
    ctx.setShadow(offset: CGSize(width: 0, height: -14), blur: 36, color: color(0x12052E, 0.45))
    ctx.addEllipse(in: orb)
    ctx.setFillColor(color(0xFFFFFF, 0.14))
    ctx.fillPath()
    ctx.restoreGState()

    ctx.saveGState()
    ctx.addEllipse(in: orb)
    ctx.clip()
    ctx.drawRadialGradient(
        gradient([(color(0xFFFFFF, 0.30), 0), (color(0xFFFFFF, 0.06), 0.7), (color(0xFFFFFF, 0.16), 1)]),
        startCenter: CGPoint(x: 430, y: 640), startRadius: 0,
        endCenter: CGPoint(x: 512, y: 512), endRadius: 290, options: [])
    // Specular highlight, upper left.
    if !small {
        ctx.drawRadialGradient(
            gradient([(color(0xFFFFFF, 0.75), 0), (color(0xFFFFFF, 0), 1)]),
            startCenter: CGPoint(x: 400, y: 690), startRadius: 0,
            endCenter: CGPoint(x: 400, y: 690), endRadius: 150, options: [])
    }
    ctx.restoreGState()

    ctx.addEllipse(in: orb.insetBy(dx: 3, dy: 3))
    ctx.setStrokeColor(color(0xFFFFFF, 0.55))
    ctx.setLineWidth(6)
    ctx.strokePath()

    // Five-bar waveform.
    let scale: CGFloat = small ? 1.3 : 1
    let heights: [CGFloat] = [140, 250, 330, 230, 160].map { $0 * scale }
    let (barWidth, gap): (CGFloat, CGFloat) = (46 * scale, 30 * scale)
    let total = CGFloat(heights.count) * barWidth + CGFloat(heights.count - 1) * gap
    ctx.saveGState()
    ctx.setShadow(offset: CGSize(width: 0, height: -4), blur: 10, color: color(0x2A0B4F, 0.35))
    ctx.setFillColor(color(0xFFFFFF, 0.97))
    for (i, h) in heights.enumerated() {
        let x = 512 - total / 2 + CGFloat(i) * (barWidth + gap)
        let bar = CGRect(x: x, y: 512 - h / 2, width: barWidth, height: h)
        ctx.addPath(CGPath(roundedRect: bar, cornerWidth: barWidth / 2, cornerHeight: barWidth / 2, transform: nil))
    }
    ctx.fillPath()
    ctx.restoreGState()

    return ctx.makeImage()!
}

func writePNG(_ image: CGImage, to url: URL) throws {
    let rep = NSBitmapImageRep(cgImage: image)
    guard let data = rep.representation(using: .png, properties: [:]) else {
        throw CocoaError(.fileWriteUnknown)
    }
    try data.write(to: url)
}

let args = CommandLine.arguments
guard args.count >= 2 else {
    FileHandle.standardError.write("usage: make-icon.swift <out.icns> [preview.png]\n".data(using: .utf8)!)
    exit(2)
}
let out = URL(fileURLWithPath: args[1])
let iconset = FileManager.default.temporaryDirectory
    .appendingPathComponent("AppIcon-\(ProcessInfo.processInfo.processIdentifier).iconset")
try? FileManager.default.removeItem(at: iconset)
try FileManager.default.createDirectory(at: iconset, withIntermediateDirectories: true)
for base in [16, 32, 128, 256, 512] {
    try writePNG(render(px: base), to: iconset.appendingPathComponent("icon_\(base)x\(base).png"))
    try writePNG(render(px: base * 2), to: iconset.appendingPathComponent("icon_\(base)x\(base)@2x.png"))
}
if args.count >= 3 {
    try writePNG(render(px: 1024), to: URL(fileURLWithPath: args[2]))
}
let iconutil = Process()
iconutil.executableURL = URL(fileURLWithPath: "/usr/bin/iconutil")
iconutil.arguments = ["-c", "icns", iconset.path, "-o", out.path]
try iconutil.run()
iconutil.waitUntilExit()
try? FileManager.default.removeItem(at: iconset)
exit(iconutil.terminationStatus)
