//
//  AccountView.swift
//  freewrite
//
//  Sign in / create account / sync status, shown as a small sheet from the
//  history sidebar.
//

import SwiftUI
import AppKit

@MainActor
struct AccountView: View {
    @ObservedObject var cloud: CloudSyncManager
    let colorScheme: ColorScheme

    @Environment(\.dismiss) private var dismiss
    @State private var email: String = ""
    @State private var password: String = ""
    @State private var hoveredButton: String? = nil

    private var textColor: Color {
        colorScheme == .light ? Color.gray : Color.gray.opacity(0.8)
    }

    private var textHoverColor: Color {
        colorScheme == .light ? Color.black : Color.white
    }

    private var canSubmit: Bool {
        !email.trimmingCharacters(in: .whitespaces).isEmpty && !password.isEmpty && !cloud.isAuthenticating
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack {
                Text("Account")
                    .font(.system(size: 13))
                    .foregroundColor(textHoverColor)
                Spacer()
                plainButton("Done") {
                    dismiss()
                }
            }

            if !cloud.isConfigured {
                Text("Sync isn't set up in this build. Your entries are saved on this Mac as always.\n\nTo turn it on, add a Supabase project URL and key to Cloud.plist (see docs/cloud-sync.md).")
                    .font(.system(size: 12))
                    .foregroundColor(textColor)
                    .fixedSize(horizontal: false, vertical: true)
            } else if let signedInEmail = cloud.email {
                signedIn(email: signedInEmail)
            } else {
                signInForm
            }

            if let error = cloud.lastError {
                Text(error)
                    .font(.system(size: 11))
                    .foregroundColor(.red.opacity(0.8))
                    .fixedSize(horizontal: false, vertical: true)
            }

            if let notice = cloud.notice {
                Text(notice)
                    .font(.system(size: 11))
                    .foregroundColor(textColor)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(24)
        .frame(width: 320)
        .background(colorScheme == .light ? Color.white : Color.black)
        .colorScheme(colorScheme)
    }

    private func signedIn(email: String) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            VStack(alignment: .leading, spacing: 4) {
                Text(email)
                    .font(.system(size: 13))
                    .foregroundColor(textHoverColor)
                    .lineLimit(1)
                Text(syncStatus)
                    .font(.system(size: 11))
                    .foregroundColor(textColor)
            }

            HStack(spacing: 12) {
                plainButton(cloud.isSyncing ? "Syncing..." : "Sync now") {
                    cloud.requestSync()
                }
                .disabled(cloud.isSyncing)

                Text("•")
                    .foregroundColor(.gray)

                plainButton("Sign out") {
                    cloud.signOut()
                }
            }
        }
    }

    private var syncStatus: String {
        if cloud.isSyncing {
            return "Syncing..."
        }
        guard let last = cloud.lastSyncedAt else {
            return "Not synced yet"
        }
        return "Last synced at " + last.formatted(date: .omitted, time: .shortened)
    }

    private var signInForm: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Sign in to keep your entries in sync across your devices.")
                .font(.system(size: 12))
                .foregroundColor(textColor)
                .fixedSize(horizontal: false, vertical: true)

            VStack(alignment: .leading, spacing: 4) {
                TextField("Email", text: $email)
                    .textFieldStyle(.plain)
                    .textContentType(.username)
                    .autocorrectionDisabled()
                    .font(.system(size: 13))
                Divider()
            }

            VStack(alignment: .leading, spacing: 4) {
                SecureField("Password", text: $password)
                    .textFieldStyle(.plain)
                    .textContentType(.password)
                    .font(.system(size: 13))
                    .onSubmit(signIn)
                Divider()
            }

            HStack(spacing: 12) {
                plainButton(cloud.isAuthenticating ? "Signing in..." : "Sign in", action: signIn)
                    .disabled(!canSubmit)

                Text("•")
                    .foregroundColor(.gray)

                plainButton("Create account") {
                    let email = trimmedEmail
                    let password = password
                    Task { @MainActor in await cloud.signUp(email: email, password: password) }
                }
                .disabled(!canSubmit)
            }
        }
    }

    private var trimmedEmail: String {
        email.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private func signIn() {
        guard canSubmit else { return }
        let email = trimmedEmail
        let password = password
        Task { @MainActor in
            await cloud.signIn(email: email, password: password)
            if cloud.email != nil {
                self.password = ""
            }
        }
    }

    private func plainButton(_ title: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Text(title)
                .font(.system(size: 13))
                .foregroundColor(hoveredButton == title ? textHoverColor : textColor)
        }
        .buttonStyle(.plain)
        .onHover { hovering in
            hoveredButton = hovering ? title : (hoveredButton == title ? nil : hoveredButton)
            if hovering {
                NSCursor.pointingHand.push()
            } else {
                NSCursor.pop()
            }
        }
    }
}
