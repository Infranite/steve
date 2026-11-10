import CoreImage
import Foundation

#if canImport(AppKit)
import AppKit
public typealias PlatformQRImage = NSImage
#elseif canImport(UIKit)
import UIKit
public typealias PlatformQRImage = UIImage
#endif

public enum NativeQRGenerator {
    /// Generates a crisp native QR code image (`NSImage` on macOS, `UIImage` on iOS) from `text` via `CIQRCodeGenerator`.
    public static func makeImage(from text: String, scale: CGFloat = 10) -> PlatformQRImage? {
        guard let data = text.data(using: .utf8),
              let filter = CIFilter(name: "CIQRCodeGenerator") else {
            return nil
        }
        filter.setValue(data, forKey: "inputMessage")
        filter.setValue("M", forKey: "inputCorrectionLevel")
        guard let output = filter.outputImage else { return nil }

        let transformed = output.transformed(by: CGAffineTransform(scaleX: scale, y: scale))
        let context = CIContext(options: [.useSoftwareRenderer: false])
        guard let cgImage = context.createCGImage(transformed, from: transformed.extent) else {
            return nil
        }
        #if canImport(AppKit)
        return NSImage(cgImage: cgImage, size: NSSize(width: cgImage.width, height: cgImage.height))
        #elseif canImport(UIKit)
        return UIImage(cgImage: cgImage)
        #endif
    }
}
