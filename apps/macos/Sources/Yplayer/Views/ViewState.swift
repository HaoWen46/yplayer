import SwiftUI

/// `@State` as the property wrapper. In this SDK `@State` is a macro whose plugin
/// (SwiftUIMacros) ships only with Xcode, not the Command Line Tools, so views write
/// `@ViewState` instead.
typealias ViewState = SwiftUI.State
