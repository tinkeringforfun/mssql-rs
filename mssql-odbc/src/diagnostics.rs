use crate::handle::*;
use crate::types::*;
use std::ptr;

pub fn get_diag_rec(
    handle_type: SQLSMALLINT,
    handle: SQLHANDLE,
    rec_number: SQLSMALLINT,
    sql_state: *mut SQLCHAR,
    native_error: *mut SQLINTEGER,
    message_text: *mut SQLCHAR,
    buffer_length: SQLSMALLINT,
    text_length: *mut SQLSMALLINT,
) -> SQLRETURN {
    if handle.is_null() {
        return SQL_INVALID_HANDLE;
    }

    let diagnostics: &[DiagRecord] = match handle_type {
        SQL_HANDLE_ENV => return SQL_NO_DATA, // env has no diagnostics in our impl
        SQL_HANDLE_DBC => {
            let conn = unsafe { &*(handle as *const Connection) };
            &conn.diagnostics
        }
        SQL_HANDLE_STMT => {
            let stmt = unsafe { &*(handle as *const Statement) };
            &stmt.diagnostics
        }
        _ => return SQL_INVALID_HANDLE,
    };

    let idx = (rec_number as usize).wrapping_sub(1);
    if idx >= diagnostics.len() {
        return SQL_NO_DATA;
    }

    let rec = &diagnostics[idx];

    // Copy SQLSTATE (5 chars + null)
    if !sql_state.is_null() {
        let state_bytes = rec.state.as_bytes();
        let copy_len = std::cmp::min(state_bytes.len(), 5);
        unsafe {
            ptr::copy_nonoverlapping(state_bytes.as_ptr(), sql_state, copy_len);
            // Pad with zeros
            for i in copy_len..6 {
                *sql_state.add(i) = 0;
            }
        }
    }

    if !native_error.is_null() {
        unsafe {
            *native_error = rec.native_error;
        }
    }

    let msg_bytes = rec.message.as_bytes();
    if !text_length.is_null() {
        unsafe {
            *text_length = msg_bytes.len() as SQLSMALLINT;
        }
    }

    if !message_text.is_null() && buffer_length > 0 {
        let copy_len = std::cmp::min(msg_bytes.len(), (buffer_length as usize).saturating_sub(1));
        unsafe {
            ptr::copy_nonoverlapping(msg_bytes.as_ptr(), message_text, copy_len);
            *message_text.add(copy_len) = 0;
        }
    }

    SQL_SUCCESS
}
