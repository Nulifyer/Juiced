# Backport https://github.com/yhirose/cpp-httplib/commit/8e702d3837b2164765ca1d98cb6d180ae4711e70.
# Keep the existing dependency ABI and model-route behavior until the broader
# upgrade can preserve long identifiers without recursive regex matching.
file(READ "${HTTPLIB_SOURCE_DIR}/httplib.h" header)
string(REPLACE "\r\n" "\n" header "${header}")
string(SHA256 actual "${header}")
if(actual STREQUAL "c15afc60c1ac4779a54775f90433e2978ceae26f1ff4c6626c0ff9d38c45257f")
    return()
endif()
if(NOT actual STREQUAL "dfbaccb76432ed6d56ddd9983fd9d262b61ba6ba0958f6b00db35c802607bd35")
    message(FATAL_ERROR "Unexpected cpp-httplib source; review the graceful-close backport")
endif()
if(HTTPLIB_VERIFY_ONLY)
    message(FATAL_ERROR
        "The cached/offline cpp-httplib source needs its graceful-close backport. "
        "Run ${CMAKE_COMMAND} -DHTTPLIB_SOURCE_DIR=${HTTPLIB_SOURCE_DIR} "
        "-P ${CMAKE_CURRENT_LIST_FILE}, then configure again.")
endif()
set(helper [=[
// Half-closes the write side and drains any in-flight/queued bytes before
// the final shutdown+close. Closing with unread data in the receive queue
// (or bytes arriving after the receive side is closed) makes the stack send
// an abortive RST instead of a graceful FIN, which can make the peer see the
// response as a failed read even though it was fully written.
inline void drain_and_close_socket(socket_t sock) noexcept {
#ifdef _WIN32
  shutdown(sock, SD_SEND);
#else
  shutdown(sock, SHUT_WR);
#endif

  char buf[CPPHTTPLIB_RECV_BUFSIZ];
  size_t total = 0;
  const auto deadline = std::chrono::steady_clock::now() +
                        std::chrono::milliseconds(100); // bound #1

  while (total < size_t(1024u * 1024u)) { // bound #2
    const auto remaining =
        std::chrono::duration_cast<std::chrono::microseconds>(
            deadline - std::chrono::steady_clock::now())
            .count();
    if (remaining <= 0) { break; }
    if (select_read(sock, 0, static_cast<time_t>(remaining)) <= 0) { break; }
    const auto n = read_socket(sock, buf, sizeof(buf), CPPHTTPLIB_RECV_FLAGS);
    if (n <= 0) { break; }
    total += static_cast<size_t>(n);
  }

  shutdown_socket(sock);
  close_socket(sock);
}

]=])
string(REPLACE "inline std::string escape_abstract_namespace_unix_domain"
    "${helper}inline std::string escape_abstract_namespace_unix_domain" header "${header}")
string(REPLACE "  detail::shutdown_socket(sock);
  detail::close_socket(sock);
  return ret;
}

inline void Server::output_log"
    "  detail::drain_and_close_socket(sock);
  return ret;
}

inline void Server::output_log" header "${header}")
string(SHA256 patched "${header}")
if(NOT patched STREQUAL "c15afc60c1ac4779a54775f90433e2978ceae26f1ff4c6626c0ff9d38c45257f")
    message(FATAL_ERROR "cpp-httplib graceful-close backport did not match its verified result")
endif()
file(WRITE "${HTTPLIB_SOURCE_DIR}/httplib.h" "${header}")
