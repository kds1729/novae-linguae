| record | run | certified | typecheck | effects | refinement | termination | complexity | properties (generative) | prove |
|---|---|---|---|---|---|---|---|---|---|
| `gi_add` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND / post[1]=SOUND | SOUND | SOUND | commutative=HELD; associative=HELD; zero_identity=EXHAUSTIVE | commutative=PROVED; associative=PROVED; zero_identity=PROVED |
| `gi_sub` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND / post[1]=SOUND | SOUND | SOUND | self_difference_is_zero=EXHAUSTIVE | self_difference_is_zero=PROVED |
| `gi_neg` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND / post[1]=SOUND | SOUND | SOUND | involution=EXHAUSTIVE | involution=PROVED |
| `gi_conj` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND / post[1]=SOUND | SOUND | SOUND | involution=EXHAUSTIVE | involution=PROVED |
| `gi_mul` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND / post[1]=SOUND | SOUND | SOUND | commutative=HELD; one_identity=EXHAUSTIVE; i_squared_is_minus_one=UNGENERATABLE; zero_annihilates=EXHAUSTIVE; associative=HELD | commutative=PROVED; one_identity=PROVED; i_squared_is_minus_one=PROVED; zero_annihilates=PROVED; associative=PROVED |
| `gi_norm` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND / post[1]=SOUND | SOUND | SOUND | zero_only_at_zero=EXHAUSTIVE; positive_off_zero=EXHAUSTIVE; conj_invariant=EXHAUSTIVE | zero_only_at_zero=PROVED; positive_off_zero=PROVED; conj_invariant=PROVED |
| `gi_norm_mul_holds` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | UNVERIFIABLE | norm_multiplicative=HELD | norm_multiplicative=PROVED |
| `gi_conj_mul_holds` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | UNVERIFIABLE | conj_multiplicative=HELD | conj_multiplicative=PROVED |
| `gi_is_unit` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | units_lie_on_an_axis=EXHAUSTIVE; the_four_units=UNGENERATABLE | units_lie_on_an_axis=PROVED; the_four_units=PROVED |
| `gi_round_div` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND / post[1]=SOUND | SOUND | SOUND | exact_on_multiples=EXHAUSTIVE; by_one=EXHAUSTIVE | exact_on_multiples=PROVED; by_one=PROVED |
| `gi_div` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND / post[1]=SOUND / post[2]=SOUND / post[3]=SOUND | SOUND | UNVERIFIABLE | by_one=EXHAUSTIVE | by_one=PROVED |
| `gi_rem` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND | SOUND | UNVERIFIABLE | by_one_is_zero=EXHAUSTIVE | by_one_is_zero=PROVED |
| `gi_euclid_holds` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | UNVERIFIABLE | remainder_smaller=HELD | remainder_smaller=PROVED |
| `gi_gcd` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | right_zero=EXHAUSTIVE | right_zero=UNSUPPORTED |
