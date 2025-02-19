//LICENSE Portions Copyright 2019-2021 ZomboDB, LLC.
//LICENSE
//LICENSE Portions Copyright 2021-2023 Technology Concepts & Design, Inc.
//LICENSE
//LICENSE Portions Copyright 2023-2023 PgCentral Foundation, Inc. <contact@pgcentral.org>
//LICENSE
//LICENSE All rights reserved.
//LICENSE
//LICENSE Use of this source code is governed by the MIT license that can be found in the LICENSE file.
//! A safe wrapper around Postgres' internal [`List`][crate::pg_sys::List] structure.
//!
//! It functions similarly to a Rust [`Vec`], including iterator support, but provides separate
//! understandings of [`List`][crate::pg_sys::List]s of [`pg_sys::Oid`]s, Integers, and Pointers.

use crate::pg_sys;
use crate::seal::Sealed;
use core::marker::PhantomData;
use core::mem;
use core::ptr::{self, NonNull};

#[cfg(any(feature = "pg13", feature = "pg14", feature = "pg15", feature = "pg16"))]
mod flat_list;
#[cfg(feature = "pg12")]
mod linked_list;

#[cfg(feature = "cshim")]
pub mod old_list;
#[cfg(feature = "cshim")]
pub use old_list::*;

/// The List type from Postgres, lifted into Rust
/// Note: you may want the ListHead type
#[derive(Debug)]
pub enum List<T> {
    Nil,
    Cons(ListHead<T>),
}

#[derive(Debug)]
pub struct ListHead<T> {
    list: NonNull<pg_sys::List>,
    _type: PhantomData<[T]>,
}

/// A strongly-typed ListCell
#[repr(transparent)]
pub struct ListCell<T> {
    // It is important that we are able to treat this union as effectively synonymous with T!
    // Thus it is important that we
    // - do not hand out the ability to construct arbitrary ListCell<T>
    // - do not offer casting between types of List<T> (which offer [ListCell<T>])
    // - do not even upgrade from pg_sys::{List, ListCell} to pgrx::list::{List, ListCell}
    // UNLESS the relevant safety invariants are appropriately handled!
    // It is not even okay to do this for FFI! We must check any *mut pg_sys::List from FFI,
    // to guarantee it has the expected type tag, otherwise the union cells may be garbage.
    cell: pg_sys::ListCell,
    _type: PhantomData<T>,
}

// Note: the size of `ListCell<T>`'s generic `T` doesn't matter,
// thus it isn't acceptable to implement Enlist for a `T` larger than `pg_sys::ListCell`.
const _: () = {
    assert!(mem::size_of::<ListCell<u128>>() == mem::size_of::<pg_sys::ListCell>());
};

/// The bound to describe a type which may be used in a Postgres List
/// It must know what an appropriate type tag is, and how to pointer-cast to itself
///
/// # Safety
/// `List<T>` relies in various ways on this being correctly implemented.
/// Incorrect implementation can lead to broken Lists, UB, or "database hilarity".
///
/// Only realistically valid to implement for union variants of pg_sys::ListCell.
/// It's not even correct to impl for `*mut T`, as `*mut T` may be a fat pointer!
pub unsafe trait Enlist: Sealed + Sized {
    /// The appropriate list tag for this type.
    const LIST_TAG: pg_sys::NodeTag;

    /// From a pointer to the pg_sys::ListCell union, obtain a pointer to Self
    /// I think this isn't actually unsafe, it just has an unsafe impl invariant?
    /// It must be implemented with ptr::addr_of! or similar, without reborrowing
    /// so that it may be used without regard to whether a pointer is write-capable
    #[doc(hidden)]
    unsafe fn apoptosis(cell: *mut pg_sys::ListCell) -> *mut Self;

    /// Set a value into a `pg_sys::ListCell`
    ///
    /// This is used instead of Enlist::apoptosis, as it guarantees initializing the union
    /// according to the rules of Rust. In practice, this is probably the same,
    /// but this way I don't have to wonder, as this is a safe function.
    #[doc(hidden)]
    fn endocytosis(cell: &mut pg_sys::ListCell, value: Self);

    #[cfg(feature = "pg12")]
    fn mitosis(cell: &pg_sys::ListCell) -> (&Self, Option<&ListCell<Self>>);

    #[cfg(feature = "pg12")]
    #[doc(hidden)]
    fn mitosis_mut(cell: &mut pg_sys::ListCell) -> (&mut Self, Option<&mut ListCell<Self>>);
}

/// Note the absence of `impl Default for ListHead`:
/// it must initialize at least 1 element to be created at all
impl<T> Default for List<T> {
    fn default() -> List<T> {
        List::Nil
    }
}

impl<T: Enlist> List<T> {
    /// Attempt to obtain a `List<T>` from a `*mut pg_sys::List`
    ///
    /// This may be somewhat confusing:
    /// A valid List of any type is the null pointer, as in the Lisp `(car, cdr)` representation.
    /// This remains true even after significant reworks of the List type in Postgres 13, which
    /// cause it to internally use a "flat array" representation.
    ///
    /// Thus, this returns `Some` even if the List is NULL, because it is `Some(List::Nil)`,
    /// and returns `None` only if the List is non-NULL but downcasting failed!
    ///
    /// # Safety
    /// This assumes the pointer is either NULL or the NodeTag is valid to read,
    /// so it is not okay to call this on pointers to deallocated or uninit data.
    ///
    /// If it returns as `Some` and the List is more than zero length, it also asserts
    /// that the entire List's `elements: *mut ListCell` is validly initialized as `T`
    /// in each ListCell and that the List is allocated from a Postgres memory context.
    ///
    /// **Note:** This memory context must last long enough for your purposes.
    /// YOU are responsible for bounding its lifetime correctly.
    pub unsafe fn downcast_ptr(ptr: *mut pg_sys::List) -> Option<List<T>> {
        match NonNull::new(ptr) {
            None => Some(List::Nil),
            Some(list) => ListHead::downcast_ptr(list).map(|head| List::Cons(head)),
        }
    }
}

impl<T> List<T> {
    #[inline]
    pub fn len(&self) -> usize {
        match self {
            List::Nil => 0,
            List::Cons(head) => head.len(),
        }
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        match self {
            List::Nil => 0,
            List::Cons(head) => head.capacity(),
        }
    }

    pub fn into_ptr(mut self) -> *mut pg_sys::List {
        self.as_mut_ptr()
    }

    pub fn as_ptr(&self) -> *const pg_sys::List {
        match self {
            List::Nil => ptr::null_mut(),
            List::Cons(head) => head.list.as_ptr(),
        }
    }

    pub fn as_mut_ptr(&mut self) -> *mut pg_sys::List {
        match self {
            List::Nil => ptr::null_mut(),
            List::Cons(head) => head.list.as_ptr(),
        }
    }
}

impl<T: Enlist> ListHead<T> {
    /// From a non-nullable pointer that points to a valid List, produce a ListHead of the correct type
    ///
    /// # Safety
    /// This assumes the NodeTag is valid to read, so it is not okay to call this on
    /// pointers to deallocated or uninit data.
    ///
    /// If it returns as `Some`, it also asserts the entire List is, across its length,
    /// validly initialized as `T` in each ListCell.
    pub unsafe fn downcast_ptr(list: NonNull<pg_sys::List>) -> Option<ListHead<T>> {
        (T::LIST_TAG == (*list.as_ptr()).type_).then_some(ListHead { list, _type: PhantomData })
    }
}
impl<T> ListHead<T> {
    #[inline]
    pub fn len(&self) -> usize {
        unsafe { self.list.as_ref().length as usize }
    }
}
