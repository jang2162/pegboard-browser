// Draws the temporary Pegboard icon (a solid square with a "P") as a PNG.
// Usage: swift icon.swift <size> <out.png>
import AppKit

let args = CommandLine.arguments
guard args.count == 3, let size = Int(args[1]) else {
    FileHandle.standardError.write("usage: icon.swift <size> <out.png>\n".data(using: .utf8)!)
    exit(2)
}
let s = CGFloat(size)
guard let rep = NSBitmapImageRep(
    bitmapDataPlanes: nil, pixelsWide: size, pixelsHigh: size, bitsPerSample: 8,
    samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
    bytesPerRow: 0, bitsPerPixel: 0)
else { exit(1) }
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
NSColor(srgbRed: 0.23, green: 0.36, blue: 0.86, alpha: 1).setFill()
NSRect(x: 0, y: 0, width: s, height: s).fill()
let font = NSFont.systemFont(ofSize: s * 0.72, weight: .bold)
let text = NSAttributedString(
    string: "P", attributes: [.font: font, .foregroundColor: NSColor.white])
let box = text.size()
text.draw(at: NSPoint(x: (s - box.width) / 2, y: (s - box.height) / 2))
NSGraphicsContext.restoreGraphicsState()
guard let png = rep.representation(using: .png, properties: [:]) else { exit(1) }
try png.write(to: URL(fileURLWithPath: args[2]))
