import AppKit
import CoreText
import VibeBuddyCore

/// The UI fonts and sprite images.
public enum BuddyAssets {
    /// Makes the fonts in `directory` (the bundle's Resources/fonts) available to this
    /// process only. Called at launch.
    public static func registerFonts(in directory: URL) {
        let fonts = (try? FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil)) ?? []
        for font in fonts where font.pathExtension == "ttf" {
            var error: Unmanaged<CFError>?
            if !CTFontManagerRegisterFontsForURL(font as CFURL, .process, &error) {
                NSLog("VibeBuddy: couldn't register \(font.lastPathComponent): \(String(describing: error?.takeRetainedValue()))")
            }
        }
    }
}

extension PixelSprite {
    /// The sprite at `scale` whole times its size, one image pixel per screen point at 1×. Draw it with
    /// `.interpolation(.none)` and at exactly this size, so nothing resamples it.
    public func image(scale: Int, tint: UInt32? = nil) -> NSImage? {
        let (outWidth, outHeight) = (width * scale, height * scale)
        let data = Data(rgba(scale: scale, tint: tint)) as CFData
        guard let provider = CGDataProvider(data: data),
              let cgImage = CGImage(width: outWidth, height: outHeight, bitsPerComponent: 8, bitsPerPixel: 32,
                                    bytesPerRow: outWidth * 4, space: CGColorSpace(name: CGColorSpace.sRGB)!,
                                    bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.last.rawValue),
                                    provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent)
        else { return nil }
        return NSImage(cgImage: cgImage, size: NSSize(width: outWidth, height: outHeight))
    }
}
