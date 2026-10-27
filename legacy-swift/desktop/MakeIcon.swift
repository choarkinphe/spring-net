import AppKit

let directory = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
for size in [16, 32, 128, 256, 512] {
    for scale in [1, 2] {
        let pixels = size * scale
        let image = NSImage(size: NSSize(width: pixels, height: pixels))
        image.lockFocus()
        let side = CGFloat(pixels)
        let inset = side * 0.05
        let box = NSRect(x: inset, y: inset, width: side - 2 * inset, height: side - 2 * inset)
        NSColor(calibratedRed: 0.10, green: 0.15, blue: 0.20, alpha: 1).setFill()
        NSBezierPath(roundedRect: box, xRadius: side * 0.2, yRadius: side * 0.2).fill()
        let blue = NSColor(calibratedRed: 0.22, green: 0.53, blue: 0.90, alpha: 1)
        blue.setStroke()
        let path = NSBezierPath()
        path.lineWidth = max(1.5, side * 0.035)
        for (i, point) in [NSPoint(x: side * 0.22, y: side * 0.30), NSPoint(x: side * 0.43, y: side * 0.64), NSPoint(x: side * 0.65, y: side * 0.40), NSPoint(x: side * 0.79, y: side * 0.70)].enumerated() {
            if i == 0 { path.move(to: point) } else { path.line(to: point) }
        }
        path.stroke()
        for point in [NSPoint(x: side * 0.22, y: side * 0.30), NSPoint(x: side * 0.43, y: side * 0.64), NSPoint(x: side * 0.65, y: side * 0.40), NSPoint(x: side * 0.79, y: side * 0.70)] {
            NSColor.white.setFill()
            NSBezierPath(ovalIn: NSRect(x: point.x - side * 0.045, y: point.y - side * 0.045, width: side * 0.09, height: side * 0.09)).fill()
        }
        image.unlockFocus()
        let bitmap = NSBitmapImageRep(data: image.tiffRepresentation!)!
        let filename = "icon_\(size)x\(size)\(scale == 2 ? "@2x" : "").png"
        try bitmap.representation(using: .png, properties: [:])!.write(to: directory.appendingPathComponent(filename))
    }
}
