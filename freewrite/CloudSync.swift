//
//  CloudSync.swift
//  freewrite
//
//  Optional account sign-in and background sync of the entry files to a
//  Supabase project. Writing stays local-first: the editor keeps saving to
//  ~/Documents/Freewrite on every keystroke, and this mirrors those files.
//  The protocol is described in docs/cloud-sync.md and shared with the
//  Windows/Linux client, so keep the two in step.
//

import Foundation
import Combine
import CryptoKit
import Security

// MARK: - Configuration

/// The Supabase project the app talks to. Read, in order, from the
/// `FREEWRITE_SUPABASE_URL` / `FREEWRITE_SUPABASE_ANON_KEY` environment
/// variables, `SupabaseURL` / `SupabaseAnonKey` keys in the app's Info.plist,
/// and the same keys in the bundled `Cloud.plist`. Without one, sync is off.
struct CloudConfig {
    let url: URL
    let anonKey: String

    static func load(bundle: Bundle = .main,
                     environment: [String: String] = ProcessInfo.processInfo.environment) -> CloudConfig? {
        var sources: [(Any?, Any?)] = [
            (environment["FREEWRITE_SUPABASE_URL"], environment["FREEWRITE_SUPABASE_ANON_KEY"]),
            (bundle.object(forInfoDictionaryKey: "SupabaseURL"), bundle.object(forInfoDictionaryKey: "SupabaseAnonKey"))
        ]
        if let plistURL = bundle.url(forResource: "Cloud", withExtension: "plist"),
           let data = try? Data(contentsOf: plistURL),
           let plist = (try? PropertyListSerialization.propertyList(from: data, format: nil)) as? [String: Any] {
            sources.append((plist["SupabaseURL"], plist["SupabaseAnonKey"]))
        }
        for (rawURL, rawKey) in sources {
            if let config = make(url: rawURL, key: rawKey) {
                return config
            }
        }
        return nil
    }

    private static func make(url rawURL: Any?, key rawKey: Any?) -> CloudConfig? {
        func clean(_ value: Any?) -> String? {
            guard let string = (value as? String)?.trimmingCharacters(in: .whitespacesAndNewlines),
                  !string.isEmpty,
                  !string.contains("$(") else { return nil }  // an unexpanded build setting
            return string
        }
        guard var urlString = clean(rawURL), let key = clean(rawKey) else { return nil }
        while urlString.hasSuffix("/") {
            urlString.removeLast()
        }
        guard let url = URL(string: urlString),
              let scheme = url.scheme?.lowercased(), scheme == "https" || scheme == "http",
              url.host != nil else { return nil }
        return CloudConfig(url: url, anonKey: key)
    }
}

// MARK: - Errors

enum CloudError: LocalizedError, Equatable {
    /// The server answered with a non-2xx status.
    case http(status: Int, message: String)
    /// The request never got an answer.
    case network(String)
    case invalidResponse
    /// The session is gone (refresh token rejected, or signed out mid-sync).
    case signedOut(String)
    /// A local file operation failed.
    case local(String)

    var errorDescription: String? {
        switch self {
        case .http(_, let message): return message
        case .network(let message): return message
        case .invalidResponse: return "Unexpected response from the sync server."
        case .signedOut(let message): return message
        case .local(let message): return message
        }
    }
}

// MARK: - Session

struct CloudSession: Codable, Equatable {
    var accessToken: String
    var refreshToken: String
    /// Unix time (seconds) when the access token expires.
    var expiresAt: TimeInterval
    /// Lowercase UUID of the Supabase user.
    var userId: String
    var email: String

    var expiresSoon: Bool {
        expiresAt - Date().timeIntervalSince1970 < 60
    }

    /// Parses a Supabase Auth session response (`/token`, or `/signup` when no
    /// email confirmation is required).
    init?(json: [String: Any]) {
        guard let accessToken = json["access_token"] as? String,
              let refreshToken = json["refresh_token"] as? String,
              let user = json["user"] as? [String: Any],
              let userId = user["id"] as? String,
              UUID(uuidString: userId) != nil else { return nil }
        self.accessToken = accessToken
        self.refreshToken = refreshToken
        if let expiresAt = (json["expires_at"] as? NSNumber)?.doubleValue {
            self.expiresAt = expiresAt
        } else if let expiresIn = (json["expires_in"] as? NSNumber)?.doubleValue {
            self.expiresAt = Date().timeIntervalSince1970 + expiresIn
        } else {
            self.expiresAt = 0
        }
        self.userId = userId.lowercased()
        self.email = user["email"] as? String ?? ""
    }
}

/// Keeps the session in the Keychain (generic password), falling back to a
/// `session.json` readable only by this user if the Keychain refuses.
enum SessionStore {
    private static var service: String { (Bundle.main.bundleIdentifier ?? "freewrite") + ".cloud" }
    private static let account = "supabase-session"
    private static var fileURL: URL { CloudPaths.supportDirectory.appendingPathComponent("session.json") }

    static func load() -> CloudSession? {
        if let data = keychainRead(), let session = try? JSONDecoder().decode(CloudSession.self, from: data) {
            return session
        }
        if let data = try? Data(contentsOf: fileURL), let session = try? JSONDecoder().decode(CloudSession.self, from: data) {
            return session
        }
        return nil
    }

    static func save(_ session: CloudSession) {
        guard let data = try? JSONEncoder().encode(session) else { return }
        if keychainWrite(data) {
            try? FileManager.default.removeItem(at: fileURL)
            return
        }
        print("Keychain unavailable, storing session in \(fileURL.path)")
        do {
            try CloudPaths.ensureSupportDirectory()
            try data.write(to: fileURL, options: .atomic)
            try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: fileURL.path)
        } catch {
            print("Error saving session: \(error)")
        }
    }

    static func clear() {
        SecItemDelete(baseQuery() as CFDictionary)
        try? FileManager.default.removeItem(at: fileURL)
    }

    private static func baseQuery() -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account
        ]
    }

    private static func keychainRead() -> Data? {
        var query = baseQuery()
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var item: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &item) == errSecSuccess else { return nil }
        return item as? Data
    }

    private static func keychainWrite(_ data: Data) -> Bool {
        let attributes: [String: Any] = [kSecValueData as String: data]
        var status = SecItemUpdate(baseQuery() as CFDictionary, attributes as CFDictionary)
        if status == errSecItemNotFound {
            var item = baseQuery()
            item[kSecValueData as String] = data
            item[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlock
            status = SecItemAdd(item as CFDictionary, nil)
        }
        return status == errSecSuccess
    }
}

enum CloudPaths {
    /// Same folder the editor uses (inside the sandbox container).
    static var entriesDirectory: URL {
        FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0].appendingPathComponent("Freewrite")
    }

    static var supportDirectory: URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0].appendingPathComponent("Freewrite")
    }

    static func ensureSupportDirectory() throws {
        try FileManager.default.createDirectory(at: supportDirectory, withIntermediateDirectories: true)
    }

    static func syncState(userId: String) -> URL {
        supportDirectory.appendingPathComponent("sync-state-\(userId).json")
    }
}

// MARK: - Wire types

struct RemoteEntry: Decodable, Equatable {
    let id: String
    let filename: String
    let content: String
    let deleted: Bool
    /// Kept as the raw string PostgREST sent (microsecond precision).
    let updatedAt: String

    enum CodingKeys: String, CodingKey {
        case id, filename, content, deleted
        case updatedAt = "updated_at"
    }

    init(id: String, filename: String, content: String, deleted: Bool, updatedAt: String) {
        self.id = id
        self.filename = filename
        self.content = content
        self.deleted = deleted
        self.updatedAt = updatedAt
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id).lowercased()
        filename = try container.decode(String.self, forKey: .filename)
        content = try container.decodeIfPresent(String.self, forKey: .content) ?? ""
        deleted = try container.decodeIfPresent(Bool.self, forKey: .deleted) ?? false
        updatedAt = try container.decode(String.self, forKey: .updatedAt)
    }
}

struct EntryUpsert: Encodable, Equatable {
    let userId: String
    let id: String
    let filename: String
    let content: String
    let deleted: Bool
    let clientUpdatedAt: String

    enum CodingKeys: String, CodingKey {
        case id, filename, content, deleted
        case userId = "user_id"
        case clientUpdatedAt = "client_updated_at"
    }
}

enum SignUpOutcome {
    case signedIn(CloudSession)
    case confirmEmail
}

/// A point in time as Postgres reports it, compared and shifted without
/// losing the microseconds `Date` formatting would drop.
struct PGTimestamp: Comparable {
    let seconds: Int64   // since 1970-01-01 UTC
    let micros: Int64    // 0..<1_000_000

    init(seconds: Int64, micros: Int64) {
        self.seconds = seconds
        self.micros = micros
    }

    /// Accepts `2026-10-29T12:34:56.123456+00:00`, `...Z`, `... 12:34:56+00`,
    /// with any number of fractional digits (or none).
    init?(_ string: String) {
        let chars = Array(string.utf8)
        var index = 0
        func number(_ count: Int) -> Int64? {
            guard index + count <= chars.count else { return nil }
            var value: Int64 = 0
            for c in chars[index..<(index + count)] {
                guard c >= 48 && c <= 57 else { return nil }
                value = value * 10 + Int64(c - 48)
            }
            index += count
            return value
        }
        func expect(_ allowed: String) -> Bool {
            guard index < chars.count, allowed.utf8.contains(chars[index]) else { return false }
            index += 1
            return true
        }
        guard let year = number(4), expect("-"), let month = number(2), expect("-"), let day = number(2),
              expect("Tt "), let hour = number(2), expect(":"), let minute = number(2), expect(":"),
              let second = number(2),
              (1...12).contains(month), (1...31).contains(day), hour < 24, minute < 60, second < 61 else { return nil }

        var micros: Int64 = 0
        if index < chars.count, chars[index] == UInt8(ascii: ".") {
            index += 1
            var digits = 0
            while index < chars.count, chars[index] >= 48, chars[index] <= 57 {
                if digits < 6 {
                    micros = micros * 10 + Int64(chars[index] - 48)
                }
                digits += 1
                index += 1
            }
            guard digits > 0 else { return nil }
            if digits < 6 {
                for _ in digits..<6 {
                    micros *= 10
                }
            }
        }

        var offset: Int64 = 0
        if index < chars.count {
            if chars[index] == UInt8(ascii: "Z") || chars[index] == UInt8(ascii: "z") {
                index += 1
            } else {
                let sign: Int64 = chars[index] == UInt8(ascii: "-") ? -1 : 1
                guard expect("+-"), let offsetHours = number(2) else { return nil }
                var offsetMinutes: Int64 = 0
                if index < chars.count {
                    _ = expect(":")
                    guard let minutes = number(2) else { return nil }
                    offsetMinutes = minutes
                }
                offset = sign * (offsetHours * 3600 + offsetMinutes * 60)
            }
        }
        guard index == chars.count else { return nil }

        let days = PGTimestamp.daysFromCivil(year: year, month: month, day: day)
        self.seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset
        self.micros = micros
    }

    func adding(seconds delta: Int64) -> PGTimestamp {
        PGTimestamp(seconds: seconds + delta, micros: micros)
    }

    /// `yyyy-MM-ddTHH:mm:ss.ffffff+00:00`
    var formatted: String {
        let days = seconds >= 0 ? seconds / 86_400 : (seconds - 86_399) / 86_400
        let secondOfDay = seconds - days * 86_400
        let (year, month, day) = PGTimestamp.civilFromDays(days)
        let micro = String(micros)
        return String(format: "%04d-%02d-%02dT%02d:%02d:%02d.", Int(year), Int(month), Int(day),
                      Int(secondOfDay / 3600), Int(secondOfDay % 3600 / 60), Int(secondOfDay % 60))
            + String(repeating: "0", count: max(0, 6 - micro.count)) + micro + "+00:00"
    }

    static func < (lhs: PGTimestamp, rhs: PGTimestamp) -> Bool {
        (lhs.seconds, lhs.micros) < (rhs.seconds, rhs.micros)
    }

    // Howard Hinnant's civil calendar algorithms (proleptic Gregorian, UTC).
    static func daysFromCivil(year: Int64, month: Int64, day: Int64) -> Int64 {
        let y = month <= 2 ? year - 1 : year
        let era = (y >= 0 ? y : y - 399) / 400
        let yoe = y - era * 400
        let mp = (month + 9) % 12
        let doy = (153 * mp + 2) / 5 + day - 1
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy
        return era * 146_097 + doe - 719_468
    }

    static func civilFromDays(_ days: Int64) -> (Int64, Int64, Int64) {
        let z = days + 719_468
        let era = (z >= 0 ? z : z - 146_096) / 146_097
        let doe = z - era * 146_097
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100)
        let mp = (5 * doy + 2) / 153
        let day = doy - (153 * mp + 2) / 5 + 1
        let month = mp < 10 ? mp + 3 : mp - 9
        let year = yoe + era * 400 + (month <= 2 ? 1 : 0)
        return (year, month, day)
    }
}

// MARK: - Supabase REST client

final class SupabaseClient {
    let config: CloudConfig
    private let urlSession: URLSession

    init(config: CloudConfig) {
        self.config = config
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 30
        configuration.timeoutIntervalForResource = 120
        self.urlSession = URLSession(configuration: configuration)
    }

    // Auth

    func signIn(email: String, password: String) async throws -> CloudSession {
        let body = try await send(authRequest("token", query: "grant_type=password",
                                              body: ["email": email, "password": password]))
        guard let session = CloudSession(json: try jsonObject(body)) else { throw CloudError.invalidResponse }
        return session
    }

    func signUp(email: String, password: String) async throws -> SignUpOutcome {
        let body = try await send(authRequest("signup", body: ["email": email, "password": password]))
        let json = try jsonObject(body)
        if json["access_token"] != nil {
            guard let session = CloudSession(json: json) else { throw CloudError.invalidResponse }
            return .signedIn(session)
        }
        return .confirmEmail
    }

    func refresh(refreshToken: String) async throws -> CloudSession {
        do {
            let body = try await send(authRequest("token", query: "grant_type=refresh_token",
                                                  body: ["refresh_token": refreshToken]))
            guard let session = CloudSession(json: try jsonObject(body)) else { throw CloudError.invalidResponse }
            return session
        } catch CloudError.http(let status, let message) where (400..<500).contains(status) {
            // A rejected refresh token means the session is gone for good.
            throw CloudError.signedOut(message)
        }
    }

    func logout(accessToken: String) async {
        var request = authRequest("logout", body: [:])
        request.setValue("Bearer \(accessToken)", forHTTPHeaderField: "Authorization")
        _ = try? await send(request)
    }

    // Entries

    /// Rows changed after `since` (or, for later pages, after the last row of
    /// the previous page), ordered by `updated_at, id`.
    func fetchChanges(accessToken: String, since: String?, after: (updatedAt: String, id: String)?,
                      limit: Int) async throws -> [RemoteEntry] {
        var query = [
            "select=id,filename,content,deleted,updated_at",
            "order=updated_at.asc,id.asc",
            "limit=\(limit)"
        ]
        if let after {
            query.append("or=" + SupabaseClient.encode(
                "(updated_at.gt.\"\(after.updatedAt)\",and(updated_at.eq.\"\(after.updatedAt)\",id.gt.\(after.id)))"))
        } else if let since {
            query.append("updated_at=" + SupabaseClient.encode("gt.\(since)"))
        }
        var request = restRequest(accessToken: accessToken, query: query.joined(separator: "&"))
        request.httpMethod = "GET"
        let body = try await send(request)
        do {
            return try JSONDecoder().decode([RemoteEntry].self, from: body)
        } catch {
            throw CloudError.invalidResponse
        }
    }

    func upsert(_ rows: [EntryUpsert], accessToken: String) async throws {
        guard !rows.isEmpty else { return }
        var request = restRequest(accessToken: accessToken, query: "on_conflict=user_id,id")
        request.httpMethod = "POST"
        request.setValue("resolution=merge-duplicates,return=minimal", forHTTPHeaderField: "Prefer")
        request.httpBody = try JSONEncoder().encode(rows)
        _ = try await send(request)
    }

    // Plumbing

    private func endpoint(_ path: String, query: String?) -> URL {
        let base = config.url.appendingPathComponent(path)
        guard let query, var components = URLComponents(url: base, resolvingAgainstBaseURL: false) else { return base }
        components.percentEncodedQuery = query
        return components.url ?? base
    }

    private func authRequest(_ path: String, query: String? = nil, body: [String: String]) -> URLRequest {
        var request = URLRequest(url: endpoint("auth/v1/" + path, query: query))
        request.httpMethod = "POST"
        request.setValue(config.anonKey, forHTTPHeaderField: "apikey")
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try? JSONSerialization.data(withJSONObject: body)
        return request
    }

    private func restRequest(accessToken: String, query: String) -> URLRequest {
        var request = URLRequest(url: endpoint("rest/v1/entries", query: query))
        request.setValue(config.anonKey, forHTTPHeaderField: "apikey")
        request.setValue("Bearer \(accessToken)", forHTTPHeaderField: "Authorization")
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        return request
    }

    private func send(_ request: URLRequest) async throws -> Data {
        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await urlSession.data(for: request)
        } catch is CancellationError {
            throw CancellationError()
        } catch let error as URLError where error.code == .cancelled {
            throw CancellationError()
        } catch {
            throw CloudError.network("Couldn't reach the sync server.")
        }
        guard let http = response as? HTTPURLResponse else { throw CloudError.invalidResponse }
        guard (200..<300).contains(http.statusCode) else {
            let json = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
            let message = ["error_description", "msg", "message", "error"]
                .lazy
                .compactMap { json?[$0] as? String }
                .first ?? "Request failed (\(http.statusCode))."
            throw CloudError.http(status: http.statusCode, message: message)
        }
        return data
    }

    private func jsonObject(_ data: Data) throws -> [String: Any] {
        guard let json = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] else {
            throw CloudError.invalidResponse
        }
        return json
    }

    private static let unreserved = CharacterSet(charactersIn:
        "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~")

    /// Percent-encodes a query value; notably `+` in timestamp offsets.
    static func encode(_ value: String) -> String {
        value.addingPercentEncoding(withAllowedCharacters: unreserved) ?? value
    }
}

// MARK: - Sync state

struct SyncState: Codable, Equatable {
    struct Record: Codable, Equatable {
        var filename: String
        /// SHA-256 (hex) of the content as of the last successful sync.
        var hash: String
    }

    /// Largest `updated_at` seen while pulling, as the server sent it.
    var cursor: String?
    /// Keyed by lowercase entry UUID.
    var entries: [String: Record] = [:]

    static func load(from url: URL) -> SyncState {
        guard let data = try? Data(contentsOf: url),
              let state = try? JSONDecoder().decode(SyncState.self, from: data) else { return SyncState() }
        return state
    }

    func save(to url: URL) throws {
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        try encoder.encode(self).write(to: url, options: .atomic)
    }
}

// MARK: - Local files

enum EntryFilename {
    private static let pattern = try! NSRegularExpression(
        pattern: "^\\[([0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12})\\]-\\[(\\d{4}-\\d{2}-\\d{2}-\\d{2}-\\d{2}-\\d{2})\\]\\.md$")

    /// The lowercase UUID and date stamp of `[uuid]-[yyyy-MM-dd-HH-mm-ss].md`.
    /// Anything else (including path separators) is rejected, since remote
    /// filenames become paths.
    static func parse(_ filename: String) -> (id: String, stamp: String)? {
        let range = NSRange(filename.startIndex..., in: filename)
        guard let match = pattern.firstMatch(in: filename, range: range),
              let idRange = Range(match.range(at: 1), in: filename),
              let stampRange = Range(match.range(at: 2), in: filename),
              let uuid = UUID(uuidString: String(filename[idRange])) else { return nil }
        return (uuid.uuidString.lowercased(), String(filename[stampRange]))
    }

    static func make(id: UUID, stamp: String) -> String {
        "[\(id.uuidString)]-[\(stamp)].md"
    }
}

func contentHash(_ data: Data) -> String {
    SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
}

/// A change sync wants to make to an entry file. `expected` is the content the
/// decision was based on (nil: the file must not exist); the change is only
/// made if the file still has it.
enum FileOperation: Equatable {
    case write(filename: String, content: String, expected: Data?)
    case delete(filename: String, expected: Data)

    var filename: String {
        switch self {
        case .write(let filename, _, _), .delete(let filename, _): return filename
        }
    }

    var expected: Data? {
        switch self {
        case .write(_, _, let expected): return expected
        case .delete(_, let expected): return expected
        }
    }
}

enum CommitResult: Equatable {
    case applied
    case preconditionFailed
    case failed(String)
}

/// The filenames a sync wrote or removed.
struct SyncChanges: Equatable {
    var changed: Set<String> = []
    var removed: Set<String> = []

    var isEmpty: Bool { changed.isEmpty && removed.isEmpty }
}

enum LocalEntryStore {
    static func apply(_ operation: FileOperation, in directory: URL) -> CommitResult {
        let url = directory.appendingPathComponent(operation.filename)
        let current: Data?
        if FileManager.default.fileExists(atPath: url.path) {
            guard let data = try? Data(contentsOf: url) else { return .preconditionFailed }
            current = data
        } else {
            current = nil
        }
        guard current == operation.expected else { return .preconditionFailed }
        do {
            switch operation {
            case .write(_, let content, _):
                try Data(content.utf8).write(to: url, options: .atomic)
            case .delete:
                try FileManager.default.removeItem(at: url)
            }
            return .applied
        } catch {
            return .failed(error.localizedDescription)
        }
    }
}

// MARK: - Sync engine

protocol CloudBackend: AnyObject {
    func fetchChanges(since: String?, after: (updatedAt: String, id: String)?, limit: Int) async throws -> [RemoteEntry]
    func upsert(_ rows: [EntryUpsert]) async throws
}

/// Supabase backend that retries once with a refreshed token on 401.
final class LiveBackend: CloudBackend {
    private let client: SupabaseClient
    private let accessToken: (_ forceRefresh: Bool) async throws -> String

    init(client: SupabaseClient, accessToken: @escaping (_ forceRefresh: Bool) async throws -> String) {
        self.client = client
        self.accessToken = accessToken
    }

    func fetchChanges(since: String?, after: (updatedAt: String, id: String)?, limit: Int) async throws -> [RemoteEntry] {
        try await authorized { token in
            try await self.client.fetchChanges(accessToken: token, since: since, after: after, limit: limit)
        }
    }

    func upsert(_ rows: [EntryUpsert]) async throws {
        try await authorized { token in
            try await self.client.upsert(rows, accessToken: token)
        }
    }

    private func authorized<T>(_ operation: (String) async throws -> T) async throws -> T {
        do {
            return try await operation(try await accessToken(false))
        } catch CloudError.http(401, _) {
            return try await operation(try await accessToken(true))
        }
    }
}

/// One sync run, as described in docs/cloud-sync.md. File reads and hashing
/// happen off the main thread; every change to a file goes through `commit`,
/// which runs on the main actor so it can't interleave with the editor's saves.
final class SyncEngine {
    typealias Commit = @MainActor ([FileOperation]) -> [CommitResult]

    static let pageSize = 500
    static let pushBatchSize = 50
    static let pullOverlap: Int64 = 60

    private let directory: URL
    private let userId: String
    private let welcome: String?
    private let backend: CloudBackend
    private let stateURL: URL
    private let commit: Commit

    init(directory: URL, userId: String, welcome: String?, backend: CloudBackend, stateURL: URL,
         commit: @escaping Commit) {
        self.directory = directory
        self.userId = userId
        self.welcome = welcome
        self.backend = backend
        self.stateURL = stateURL
        self.commit = commit
    }

    func run() async throws {
        var state = SyncState.load(from: stateURL)
        try await pull(&state)
        try Task.checkCancellation()
        try await push(&state)
    }

    // Pull

    private enum Action {
        /// Follow a remote deletion.
        case delete(id: String)
        /// Take the remote version (new here, or unchanged here since the last sync).
        case take(id: String, filename: String, remoteHash: String)
        /// Save the remote version beside ours as a new entry.
        case keepBoth(id: String, original: String, remoteHash: String)
    }

    private func pull(_ state: inout SyncState) async throws {
        var local = try scanDirectory().reduce(into: [String: String]()) { index, file in
            if index[file.id] == nil {
                index[file.id] = file.filename
            }
        }
        let since = state.cursor.flatMap(PGTimestamp.init)?.adding(seconds: -SyncEngine.pullOverlap).formatted
        var after: (updatedAt: String, id: String)? = nil

        while true {
            try Task.checkCancellation()
            let rows = try await backend.fetchChanges(since: since, after: after, limit: SyncEngine.pageSize)
            guard let last = rows.last else { break }
            try await apply(rows, to: &state, local: &local)

            var cursor = state.cursor.flatMap(PGTimestamp.init)
            for row in rows {
                if let time = PGTimestamp(row.updatedAt), cursor.map({ time > $0 }) ?? true {
                    cursor = time
                    state.cursor = row.updatedAt
                }
            }
            try save(state)
            if rows.count < SyncEngine.pageSize { break }
            after = (last.updatedAt, last.id)
        }
    }

    private func apply(_ rows: [RemoteEntry], to state: inout SyncState, local: inout [String: String]) async throws {
        var operations: [FileOperation] = []
        var actions: [Action] = []

        func keepBoth(id: String, original: String, content: String, stamp: String, remoteHash: String) {
            if isDisposable(content) {
                // Nothing worth keeping on their side; ours wins.
                state.entries[id] = SyncState.Record(filename: original, hash: remoteHash)
                return
            }
            let copy = EntryFilename.make(id: UUID(), stamp: stamp)
            operations.append(.write(filename: copy, content: content, expected: nil))
            actions.append(.keepBoth(id: id, original: original, remoteHash: remoteHash))
        }

        for row in rows {
            guard let parsed = EntryFilename.parse(row.filename), parsed.id == row.id else {
                print("Skipping remote entry with unexpected filename: \(row.filename)")
                continue
            }
            let id = row.id
            let record = state.entries[id]
            let localName = local[id] ?? row.filename
            let localData = try? Data(contentsOf: directory.appendingPathComponent(localName))
            let localHash = localData.map(contentHash)
            let remoteHash = contentHash(Data(row.content.utf8))

            if row.deleted {
                guard let localData, let localHash else {
                    state.entries[id] = nil
                    continue
                }
                // Unchanged here since the last sync: follow the deletion.
                // Edited here since: keep it, and the push resurrects it.
                if record?.hash == localHash {
                    operations.append(.delete(filename: localName, expected: localData))
                    actions.append(.delete(id: id))
                }
                continue
            }

            // Nothing changed remotely since we last synced this entry (this
            // also covers a local deletion, which the push turns into a tombstone).
            if record?.hash == remoteHash {
                continue
            }

            guard let localData, let localHash else {
                operations.append(.write(filename: row.filename, content: row.content, expected: nil))
                actions.append(.take(id: id, filename: row.filename, remoteHash: remoteHash))
                continue
            }

            if localHash == remoteHash {
                state.entries[id] = SyncState.Record(filename: localName, hash: remoteHash)
                continue
            }

            if record?.hash == localHash {
                operations.append(.write(filename: localName, content: row.content, expected: localData))
                actions.append(.take(id: id, filename: localName, remoteHash: remoteHash))
                continue
            }

            // Both sides changed: keep ours in place, theirs beside it.
            keepBoth(id: id, original: localName, content: row.content, stamp: parsed.stamp, remoteHash: remoteHash)
        }

        var failure: String?
        var round = 0
        while !operations.isEmpty {
            let batch = operations
            let batchActions = actions
            operations = []
            actions = []
            round += 1
            let results = await commit(batch)

            for (index, action) in batchActions.enumerated() {
                let result = index < results.count ? results[index] : .preconditionFailed
                switch (action, result) {
                case (.delete(let id), .applied):
                    state.entries[id] = nil
                    local[id] = nil
                case (.delete, .preconditionFailed):
                    // Edited meanwhile; the push resurrects it.
                    break
                case (.take(let id, let filename, let remoteHash), .applied):
                    state.entries[id] = SyncState.Record(filename: filename, hash: remoteHash)
                    local[id] = filename
                case (.take(let id, let filename, let remoteHash), .preconditionFailed):
                    // The file changed while we were deciding (a keystroke):
                    // treat it as a conflict so neither version is lost.
                    if case .write(_, let content, _) = batch[index],
                       let stamp = EntryFilename.parse(filename)?.stamp, round == 1 {
                        keepBoth(id: id, original: filename, content: content, stamp: stamp, remoteHash: remoteHash)
                    }
                case (.keepBoth(let id, let original, let remoteHash), .applied):
                    // Record theirs as seen, so ours is pushed over it.
                    state.entries[id] = SyncState.Record(filename: original, hash: remoteHash)
                case (.keepBoth(_, let original, _), .preconditionFailed):
                    failure = failure ?? "Couldn't save a conflicting copy of \(original)."
                case (_, .failed(let message)):
                    failure = failure ?? message
                }
            }
        }

        if let failure {
            try? save(state)
            throw CloudError.local(failure)
        }
    }

    // Push

    private func push(_ state: inout SyncState) async throws {
        var rows: [EntryUpsert] = []
        var pushed: [String: SyncState.Record] = [:]
        var present = Set<String>()
        let timestamp = ISO8601DateFormatter()
        timestamp.formatOptions = [.withInternetDateTime, .withFractionalSeconds]

        for file in try scanDirectory() {
            present.insert(file.id)
            guard let data = try? Data(contentsOf: file.url) else { continue }
            let hash = contentHash(data)
            let record = state.entries[file.id]
            if record?.hash == hash { continue }
            guard let content = String(data: data, encoding: .utf8) else { continue }
            // Blank pages and the untouched guide stay on this device.
            if record == nil && isDisposable(content) { continue }
            if pushed[file.id] != nil { continue }
            rows.append(EntryUpsert(userId: userId, id: file.id, filename: file.filename, content: content,
                                    deleted: false, clientUpdatedAt: timestamp.string(from: file.modified)))
            pushed[file.id] = SyncState.Record(filename: file.filename, hash: hash)
        }

        let now = timestamp.string(from: Date())
        for (id, record) in state.entries.sorted(by: { $0.key < $1.key }) where !present.contains(id) {
            rows.append(EntryUpsert(userId: userId, id: id, filename: record.filename, content: "",
                                    deleted: true, clientUpdatedAt: now))
        }

        var start = 0
        while start < rows.count {
            try Task.checkCancellation()
            let batch = Array(rows[start..<min(start + SyncEngine.pushBatchSize, rows.count)])
            try await backend.upsert(batch)
            for row in batch {
                state.entries[row.id] = row.deleted ? nil : pushed[row.id]
            }
            try save(state)
            start += batch.count
        }
    }

    // Helpers

    private struct LocalFile {
        let id: String
        let filename: String
        let url: URL
        let modified: Date
    }

    private func scanDirectory() throws -> [LocalFile] {
        var isDirectory: ObjCBool = false
        guard FileManager.default.fileExists(atPath: directory.path, isDirectory: &isDirectory), isDirectory.boolValue else {
            // Never treat a missing folder as "everything was deleted".
            throw CloudError.local("The Freewrite folder is missing.")
        }
        let urls = try FileManager.default.contentsOfDirectory(
            at: directory, includingPropertiesForKeys: [.contentModificationDateKey], options: [.skipsHiddenFiles])
        return urls.compactMap { url in
            let filename = url.lastPathComponent
            guard let parsed = EntryFilename.parse(filename) else { return nil }
            let modified = (try? url.resourceValues(forKeys: [.contentModificationDateKey]))?.contentModificationDate ?? Date()
            return LocalFile(id: parsed.id, filename: filename, url: url, modified: modified)
        }
        .sorted { $0.filename < $1.filename }
    }

    private func isDisposable(_ content: String) -> Bool {
        let trimmed = content.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty { return true }
        guard let welcome else { return false }
        return content == welcome || trimmed == welcome.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private func save(_ state: SyncState) throws {
        do {
            try state.save(to: stateURL)
        } catch {
            throw CloudError.local("Couldn't save sync state: \(error.localizedDescription)")
        }
    }
}

// MARK: - Manager

@MainActor
final class CloudSyncManager: ObservableObject {
    static let shared = CloudSyncManager()

    @Published private(set) var isConfigured: Bool
    /// Email of the signed-in account, nil when signed out.
    @Published private(set) var email: String?
    @Published private(set) var isSyncing = false
    @Published private(set) var isAuthenticating = false
    @Published private(set) var lastSyncedAt: Date?
    @Published var lastError: String?
    /// Informational message, e.g. "check your email".
    @Published var notice: String?
    /// Bumped whenever sync wrote or removed entry files.
    @Published private(set) var changeCounter = 0

    /// Called on the main actor, synchronously after sync changed files on
    /// disk, before any further keystroke can be saved.
    var onFilesChanged: ((SyncChanges) -> Void)?
    /// The open entry's filename and the editor's current text, so sync never
    /// replaces a file whose newest text hasn't been saved yet.
    var editorSnapshot: (() -> (filename: String, text: String)?)?

    var isSignedIn: Bool { session != nil }

    private let client: SupabaseClient?
    private let directory: URL
    private let welcome: String?
    private var session: CloudSession?
    private var syncTask: Task<Void, Never>?
    private var syncAgain = false
    /// Bumped on sign-out so a cancelled sync loop can't clear a newer one.
    private var syncGeneration = 0
    private var periodicTask: Task<Void, Never>?
    private var refreshTask: Task<CloudSession, Error>?
    private static let syncInterval: UInt64 = 30

    init(config: CloudConfig? = CloudConfig.load(), directory: URL = CloudPaths.entriesDirectory) {
        self.client = config.map(SupabaseClient.init)
        self.directory = directory
        self.isConfigured = config != nil
        if let url = Bundle.main.url(forResource: "default", withExtension: "md"),
           let guide = try? String(contentsOf: url, encoding: .utf8) {
            self.welcome = "\n\n" + guide
        } else {
            self.welcome = nil
        }
        if config != nil {
            session = SessionStore.load()
            email = session?.email
        }
    }

    /// Starts background syncing if a session was restored. Call once the
    /// editor has loaded its entries.
    func start() {
        guard isSignedIn else { return }
        startPeriodicSync()
        requestSync()
    }

    // Account

    func signIn(email: String, password: String) async {
        guard let client else { return }
        await authenticate {
            .signedIn(try await client.signIn(email: email, password: password))
        }
    }

    func signUp(email: String, password: String) async {
        guard let client else { return }
        await authenticate {
            try await client.signUp(email: email, password: password)
        }
    }

    func signOut() {
        guard let current = session else { return }
        endSession(message: nil)
        if let client {
            Task { await client.logout(accessToken: current.accessToken) }
        }
    }

    private func authenticate(_ attempt: () async throws -> SignUpOutcome) async {
        isAuthenticating = true
        lastError = nil
        notice = nil
        defer { isAuthenticating = false }
        do {
            switch try await attempt() {
            case .signedIn(let newSession):
                session = newSession
                SessionStore.save(newSession)
                email = newSession.email
                lastSyncedAt = nil
                start()
            case .confirmEmail:
                notice = "Check your email to confirm your account, then sign in."
            }
        } catch {
            lastError = error.localizedDescription
        }
    }

    private func endSession(message: String?) {
        periodicTask?.cancel()
        periodicTask = nil
        syncTask?.cancel()
        syncTask = nil
        syncAgain = false
        syncGeneration += 1
        session = nil
        SessionStore.clear()
        email = nil
        lastSyncedAt = nil
        lastError = message
    }

    // Syncing

    /// Runs a sync soon; if one is already running, runs another right after.
    func requestSync() {
        guard isSignedIn, client != nil else { return }
        if syncTask != nil {
            syncAgain = true
            return
        }
        let generation = syncGeneration
        syncTask = Task { [weak self] in
            await self?.syncLoop(generation: generation)
        }
    }

    /// Best effort before the app quits: sync once more, waiting at most `timeout` seconds.
    func syncBeforeQuit(timeout: TimeInterval) async {
        guard isSignedIn else { return }
        requestSync()
        guard let task = syncTask else { return }
        await withTaskGroup(of: Void.self) { group in
            group.addTask { await task.value }
            group.addTask { try? await Task.sleep(nanoseconds: UInt64(timeout * 1_000_000_000)) }
            await group.next()
            group.cancelAll()
        }
    }

    private func startPeriodicSync() {
        guard periodicTask == nil else { return }
        periodicTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: CloudSyncManager.syncInterval * 1_000_000_000)
                guard !Task.isCancelled else { break }
                self?.requestSync()
            }
        }
    }

    private func syncLoop(generation: Int) async {
        repeat {
            syncAgain = false
            await syncOnce()
        } while syncAgain && isSignedIn && !Task.isCancelled
        if generation == syncGeneration {
            syncTask = nil
        }
    }

    private func syncOnce() async {
        guard let client, let current = session else { return }
        isSyncing = true
        defer { isSyncing = false }
        let userId = current.userId
        let backend = LiveBackend(client: client) { [weak self] forceRefresh in
            guard let self else { throw CancellationError() }
            return try await self.accessToken(forceRefresh: forceRefresh, userId: userId)
        }
        let engine = SyncEngine(
            directory: directory, userId: userId, welcome: welcome, backend: backend,
            stateURL: CloudPaths.syncState(userId: userId),
            commit: { [weak self] operations in
                self?.commit(operations) ?? operations.map { _ in .preconditionFailed }
            })
        do {
            try await engine.run()
            guard session?.userId == userId else { return }
            lastSyncedAt = Date()
            lastError = nil
        } catch is CancellationError {
        } catch CloudError.signedOut {
            if session?.userId == userId {
                endSession(message: "Your session expired. Sign in again to keep syncing.")
            }
        } catch {
            guard session?.userId == userId else { return }
            lastError = error.localizedDescription
            print("Sync failed: \(error)")
        }
    }

    private func accessToken(forceRefresh: Bool, userId: String) async throws -> String {
        guard let client, let current = session, current.userId == userId else {
            throw CancellationError()
        }
        if !forceRefresh && !current.expiresSoon {
            return current.accessToken
        }
        if let refreshTask {
            return try await refreshTask.value.accessToken
        }
        let task = Task { try await client.refresh(refreshToken: current.refreshToken) }
        refreshTask = task
        defer { refreshTask = nil }
        let fresh = try await task.value
        guard session?.userId == userId else { throw CancellationError() }
        session = fresh
        SessionStore.save(fresh)
        email = fresh.email
        return fresh.accessToken
    }

    /// Applies sync's file changes on the main actor, so they can't interleave
    /// with the editor's own saves, then tells the editor right away.
    private func commit(_ operations: [FileOperation]) -> [CommitResult] {
        let editor = editorSnapshot?()
        var changes = SyncChanges()
        let results = operations.map { operation -> CommitResult in
            if let editor, editor.filename == operation.filename, Data(editor.text.utf8) != operation.expected {
                // The editor holds text that differs from what sync decided on.
                return .preconditionFailed
            }
            let result = LocalEntryStore.apply(operation, in: directory)
            if result == .applied {
                switch operation {
                case .write(let filename, _, _): changes.changed.insert(filename)
                case .delete(let filename, _): changes.removed.insert(filename)
                }
            }
            return result
        }
        if !changes.isEmpty {
            changeCounter += 1
            onFilesChanged?(changes)
        }
        return results
    }
}
