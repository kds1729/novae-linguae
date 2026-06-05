-- | Sample module for exercising nl-ingest-hs. Not part of Novae Linguae itself.
module Data.Sample
  ( double
  , mapMaybe
  , compose
  , konst
  , Pair(..)      -- a type export, must be ignored
  , (<+>)
  ) where

import Data.Maybe (Maybe (..))

-- A simple unary function.
double :: Int -> Int
double n = n + n

-- A polymorphic, multi-line signature with a context.
mapMaybe
  :: (a -> Maybe b)
  -> [a]
  -> [b]
mapMaybe f xs = [y | x <- xs, Just y <- [f x]]

-- Higher-order: a function argument is parenthesised (nested arrow not counted).
compose :: (b -> c) -> (a -> b) -> a -> c
compose g f = \x -> g (f x)

-- Two names sharing one signature; a constant (arity 0) plus a value.
konst :: a -> b -> a
konst x _ = x

-- A user-defined operator export.
(<+>) :: Semigroup a => a -> a -> a
(<+>) = (<>)

-- Not exported (only Data.Sample exports above): should be skipped unless --include-private.
secretHelper :: Int -> Int
secretHelper x = x * 2

data Pair a b = Pair a b
